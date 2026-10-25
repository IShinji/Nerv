//! Supervised JSON-RPC client for the Python brain subprocess.
//!
//! Both the gateway and the desktop GUI drive the same brain over the same
//! newline-delimited JSON-RPC protocol, so the client lives here rather than
//! being reimplemented per host.
//!
//! Three properties matter and are covered by the tests below:
//!
//! * **No orphaned callers.** When the brain's stdout closes, every request
//!   still waiting is failed immediately. Leaving the oneshot senders parked in
//!   the pending map made a brain crash hang the caller forever.
//! * **Supervision.** A transport failure or call timeout respawns the brain,
//!   up to `restart_max` times per `restart_window`, with exponential backoff.
//!   A request is only re-sent when it was never written to the dead process;
//!   otherwise it may already have had side effects and the error is returned.
//! * **Bounded waits.** Every call has a timeout, so a wedged brain surfaces an
//!   error (and gets replaced) instead of stalling the channel it came from.
//! * **No orphans.** The brain runs in its own process group and the whole
//!   group is killed on stop/drop, so `uv run python` leaves no stray python.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, info, warn};

use crate::ipc::{JsonRpcRequest, JsonRpcResponse};

/// How long to wait for the brain's first `ping` after spawning it.
///
/// Generous: a cold `uv run` may resolve and build the environment first.
const READY_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Error)]
pub enum BrainError {
    #[error("brain process is not running")]
    NotRunning,
    #[error("brain transport failure: {0}")]
    Transport(String),
    #[error("brain call `{method}` timed out after {timeout:?}")]
    Timeout { method: String, timeout: Duration },
    #[error("brain returned an error: {0}")]
    Rpc(String),
    #[error("failed to spawn brain process: {0}")]
    Spawn(String),
    #[error("brain exhausted its {0} restart attempt(s)")]
    RestartsExhausted(u32),
}

impl BrainError {
    /// Whether the failure means the process is gone and a respawn may help.
    ///
    /// Timeouts are handled separately by `call`: the brain is replaced but
    /// the request is never re-sent.
    fn is_transport(&self) -> bool {
        matches!(self, BrainError::NotRunning | BrainError::Transport(_))
    }
}

/// Everything needed to spawn the brain.
#[derive(Debug, Clone)]
pub struct BrainSpawnConfig {
    /// Program plus leading arguments, e.g. `"uv run python"`.
    pub command: String,
    /// Arguments appended after `command`, e.g. `["-m", "nerv"]`.
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    pub env: Vec<(String, String)>,
    /// Maximum restarts within one `restart_window`.
    pub restart_max: u32,
    /// The restart counter resets once the brain has run this long unrestarted.
    pub restart_window: Duration,
    /// First restart delay; doubles per consecutive restart, capped at 30 s.
    pub restart_backoff: Duration,
    pub call_timeout: Duration,
}

impl BrainSpawnConfig {
    /// Config for the standard `<python_command> -m <module>` invocation.
    pub fn python(python_command: &str, module: &str, brain_dir: impl Into<PathBuf>) -> Self {
        Self {
            command: python_command.to_string(),
            args: vec!["-m".to_string(), module.to_string()],
            working_dir: brain_dir.into(),
            env: Vec::new(),
            restart_max: 3,
            restart_window: Duration::from_secs(600),
            restart_backoff: Duration::from_secs(1),
            call_timeout: Duration::from_secs(300),
        }
    }

    pub fn with_env(mut self, key: &str, value: impl AsRef<str>) -> Self {
        self.env.push((key.to_string(), value.as_ref().to_string()));
        self
    }

    pub fn with_restart_max(mut self, restart_max: u32) -> Self {
        self.restart_max = restart_max;
        self
    }

    pub fn with_restart_window(mut self, window: Duration) -> Self {
        self.restart_window = window;
        self
    }

    pub fn with_restart_backoff(mut self, backoff: Duration) -> Self {
        self.restart_backoff = backoff;
        self
    }

    pub fn with_call_timeout(mut self, call_timeout: Duration) -> Self {
        self.call_timeout = call_timeout;
        self
    }
}

type PendingReply = Result<serde_json::Value, BrainError>;
type PendingMap = Arc<StdMutex<HashMap<u64, oneshot::Sender<PendingReply>>>>;

/// One live brain process and the requests outstanding against *it*.
///
/// The pending map is per-process on purpose: a restart must not let the dying
/// process's reader task cancel requests already issued to its replacement.
struct Process {
    child: Child,
    stdin: ChildStdin,
    pending: PendingMap,
    /// Kills the whole process group when dropped.
    group: ProcessGroup,
}

/// The brain's process group. `uv run python` makes python a grandchild, so
/// killing only the direct child would orphan it.
struct ProcessGroup(Option<u32>);

impl ProcessGroup {
    fn kill(&self) {
        #[cfg(unix)]
        if let Some(pgid) = self.0.and_then(|p| i32::try_from(p).ok()) {
            // SAFETY: killpg only sends a signal; the pgid is our own child's.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Restart bookkeeping: count within the current window and when it began.
struct RestartState {
    used: u32,
    last_restart: Option<Instant>,
}

/// Manages the Python brain subprocess and JSON-RPC communication.
pub struct BrainClient {
    config: BrainSpawnConfig,
    process: Mutex<Option<Process>>,
    next_id: AtomicU64,
    /// Bumped on every successful spawn, so concurrent failures from the same
    /// dead process trigger exactly one restart between them.
    generation: AtomicU64,
    restart_state: StdMutex<RestartState>,
    restart_lock: Mutex<()>,
}

impl BrainClient {
    pub fn new(config: BrainSpawnConfig) -> Self {
        Self {
            config,
            process: Mutex::new(None),
            next_id: AtomicU64::new(1),
            generation: AtomicU64::new(0),
            restart_state: StdMutex::new(RestartState {
                used: 0,
                last_restart: None,
            }),
            restart_lock: Mutex::new(()),
        }
    }

    /// Start the brain subprocess and wait until it answers `ping`.
    pub async fn start(&self) -> Result<(), BrainError> {
        self.spawn().await
    }

    /// Restarts counted in the current window (resets after a healthy period).
    pub fn restarts_used(&self) -> u32 {
        let mut state = self.restart_state.lock().expect("restart state poisoned");
        self.expire_window(&mut state);
        state.used
    }

    fn expire_window(&self, state: &mut RestartState) {
        if let Some(last) = state.last_restart {
            if last.elapsed() >= self.config.restart_window {
                state.used = 0;
                state.last_restart = None;
            }
        }
    }

    async fn spawn(&self) -> Result<(), BrainError> {
        info!(
            "Starting brain process: {} {} (cwd: {})",
            self.config.command,
            self.config.args.join(" "),
            self.config.working_dir.display()
        );

        // Support multi-word commands like "uv run python".
        let mut parts = self.config.command.split_whitespace();
        let program = parts
            .next()
            .ok_or_else(|| BrainError::Spawn("command is empty".to_string()))?;

        let mut cmd = Command::new(program);
        cmd.args(parts)
            .args(&self.config.args)
            .current_dir(&self.config.working_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit()); // stderr → tracing output

        for (key, value) in &self.config.env {
            cmd.env(key, value);
        }
        // Killing the handle on drop stops a restarted brain from lingering;
        // the process group (below) covers grandchildren such as python under uv.
        cmd.kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn().map_err(|e| BrainError::Spawn(e.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| BrainError::Spawn("could not capture stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| BrainError::Spawn("could not capture stdout".to_string()))?;

        let group = ProcessGroup(child.id());
        let pending: PendingMap = Arc::new(StdMutex::new(HashMap::new()));
        tokio::spawn(read_brain_stdout(BufReader::new(stdout), pending.clone()));
        *self.process.lock().await = Some(Process {
            child,
            stdin,
            pending,
            group,
        });

        info!("Waiting for brain process readiness...");
        let sent = AtomicBool::new(false);
        let readiness = tokio::time::timeout(
            READY_TIMEOUT,
            self.call_once("ping", serde_json::json!({}), &sent),
        )
        .await
        .map_err(|_| BrainError::Timeout {
            method: "ping".to_string(),
            timeout: READY_TIMEOUT,
        })??;

        if readiness.get("status").and_then(|v| v.as_str()) != Some("ok") {
            return Err(BrainError::Spawn(format!(
                "unexpected readiness response: {readiness}"
            )));
        }

        self.generation.fetch_add(1, Ordering::AcqRel);
        info!("Brain process started successfully");
        Ok(())
    }

    /// Send a JSON-RPC request.
    ///
    /// If the brain is down *before* the request was written, it is respawned
    /// and the request sent once more. If the request was already written, it
    /// is never re-sent (it may have had side effects): the brain is replaced
    /// for the next caller and the error is returned. A timeout likewise
    /// replaces the wedged brain.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, BrainError> {
        let generation = self.generation.load(Ordering::Acquire);

        let sent = AtomicBool::new(false);
        let err = match self.call_with_timeout(method, params.clone(), &sent).await {
            Ok(value) => return Ok(value),
            Err(err) => err,
        };

        match &err {
            BrainError::Timeout { .. } => {
                warn!("Brain call `{method}` timed out; replacing the brain");
                if let Err(restart_err) = self.restart(generation).await {
                    warn!("Could not replace the wedged brain: {restart_err}");
                }
                Err(err)
            }
            e if e.is_transport() && !sent.load(Ordering::Acquire) => {
                warn!("Brain call `{method}` failed before sending ({err}); restarting");
                self.restart(generation).await?;
                let sent = AtomicBool::new(false);
                self.call_with_timeout(method, params, &sent).await
            }
            e if e.is_transport() => {
                warn!("Brain call `{method}` failed after sending ({err}); not re-sending");
                self.restart(generation).await?;
                Err(err)
            }
            _ => Err(err),
        }
    }

    async fn call_with_timeout(
        &self,
        method: &str,
        params: serde_json::Value,
        sent: &AtomicBool,
    ) -> Result<serde_json::Value, BrainError> {
        match tokio::time::timeout(
            self.config.call_timeout,
            self.call_once(method, params, sent),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(BrainError::Timeout {
                method: method.to_string(),
                timeout: self.config.call_timeout,
            }),
        }
    }

    /// One request/response round trip, with no supervision or timeout.
    /// `sent` is set once the request line has been fully written.
    async fn call_once(
        &self,
        method: &str,
        params: serde_json::Value,
        sent: &AtomicBool,
    ) -> Result<serde_json::Value, BrainError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let line = JsonRpcRequest::new(method, params, id)
            .to_line()
            .map_err(|e| BrainError::Transport(e.to_string()))?;

        let (tx, rx) = oneshot::channel();

        debug!("→ Brain: {}", line.trim());

        {
            // Holding the process lock across the write keeps two concurrent
            // callers from interleaving halves of a line on the brain's stdin.
            let mut guard = self.process.lock().await;
            let process = guard.as_mut().ok_or(BrainError::NotRunning)?;
            process
                .pending
                .lock()
                .expect("pending map poisoned")
                .insert(id, tx);

            // A partial write may already have reached the brain, so count the
            // attempt as sent from here on.
            sent.store(true, Ordering::Release);
            if let Err(e) = write_line(&mut process.stdin, &line).await {
                // Nobody will ever answer this id — do not leak the slot.
                process
                    .pending
                    .lock()
                    .expect("pending map poisoned")
                    .remove(&id);
                return Err(e);
            }
        }

        // The reader task drops every sender when the brain dies, which turns
        // this await into an error instead of an indefinite hang.
        match rx.await {
            Ok(reply) => reply,
            Err(_) => Err(BrainError::Transport(format!(
                "brain closed before responding to request id {id}"
            ))),
        }
    }

    /// Respawn the brain, unless another caller already replaced this generation.
    async fn restart(&self, failed_generation: u64) -> Result<(), BrainError> {
        let _guard = self.restart_lock.lock().await;

        if self.generation.load(Ordering::Acquire) != failed_generation {
            debug!("Brain was already restarted by another caller");
            return Ok(());
        }

        let used = {
            let mut state = self.restart_state.lock().expect("restart state poisoned");
            self.expire_window(&mut state);
            if state.used >= self.config.restart_max {
                return Err(BrainError::RestartsExhausted(self.config.restart_max));
            }
            state.used += 1;
            state.last_restart = Some(Instant::now());
            state.used
        };

        // Exponential backoff: backoff, 2x, 4x ... capped at 30 s.
        let delay = self
            .config
            .restart_backoff
            .saturating_mul(1u32 << (used - 1).min(16))
            .min(Duration::from_secs(30));
        info!(
            "Restarting brain process (attempt {used}/{}) in {delay:?}",
            self.config.restart_max
        );
        self.terminate().await;
        tokio::time::sleep(delay).await;
        self.spawn().await
    }

    /// Stop the brain subprocess gracefully.
    pub async fn stop(&self) -> Result<(), BrainError> {
        self.terminate().await;
        Ok(())
    }

    async fn terminate(&self) {
        let Some(Process {
            mut child,
            stdin,
            pending,
            group,
        }) = self.process.lock().await.take()
        else {
            return;
        };

        // Dropping stdin signals EOF, which is how the brain exits its loop.
        drop(stdin);

        match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(status)) => info!("Brain process exited: {status}"),
            Ok(Err(e)) => warn!("Error waiting for brain process: {e}"),
            Err(_) => {
                warn!("Brain process did not exit in time, killing");
                child.kill().await.ok();
            }
        }
        // Also reap anything the brain left behind (e.g. python under uv).
        drop(group);

        // Whatever the reader missed, fail it now rather than leaving it parked.
        fail_all_pending(&pending, "brain process stopped");
    }

    /// Whether the subprocess is still alive.
    pub async fn is_running(&self) -> bool {
        match self.process.lock().await.as_mut() {
            Some(process) => matches!(process.child.try_wait(), Ok(None)),
            None => false,
        }
    }
}

async fn write_line(stdin: &mut ChildStdin, line: &str) -> Result<(), BrainError> {
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| BrainError::Transport(e.to_string()))?;
    stdin
        .flush()
        .await
        .map_err(|e| BrainError::Transport(e.to_string()))
}

/// Fail every waiting request with `reason`.
fn fail_all_pending(pending: &PendingMap, reason: &str) {
    let waiting: Vec<_> = pending
        .lock()
        .expect("pending map poisoned")
        .drain()
        .collect();
    if waiting.is_empty() {
        return;
    }
    warn!(
        "Failing {} in-flight brain request(s): {reason}",
        waiting.len()
    );
    for (_id, tx) in waiting {
        // Transport, not Rpc: the process is gone, so this is retryable.
        let _ = tx.send(Err(BrainError::Transport(reason.to_string())));
    }
}

/// Route brain stdout lines to their waiting callers until the pipe closes.
async fn read_brain_stdout<R>(mut reader: BufReader<R>, pending: PendingMap)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => {
                debug!("Brain process stdout closed");
                break;
            }
            Ok(_) => dispatch_line(line.trim(), &pending),
            Err(e) => {
                warn!("Error reading from brain stdout: {e}");
                break;
            }
        }
    }

    // The process is gone; unblock everyone still waiting on it.
    fail_all_pending(&pending, "stdout closed");
}

fn dispatch_line(trimmed: &str, pending: &PendingMap) {
    if trimmed.is_empty() {
        return;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        debug!("← Brain (raw): {trimmed}");
        return;
    };

    // Notifications carry a method instead of an id.
    if value.get("method").and_then(|m| m.as_str()) == Some("notify") {
        let message = value
            .get("params")
            .and_then(|p| p.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or(trimmed);
        info!("⚡[Brain]: {message}");
        return;
    }

    let Ok(response) = serde_json::from_value::<JsonRpcResponse>(value) else {
        info!("⚡ {trimmed}");
        return;
    };

    let id = response.id;
    let waiter = pending.lock().expect("pending map poisoned").remove(&id);
    let Some(tx) = waiter else {
        debug!("Response for unknown or expired request id: {id}");
        return;
    };

    let payload = response
        .into_result()
        .map_err(|e| BrainError::Rpc(e.to_string()));
    if let Err(error) = &payload {
        warn!("Brain returned error for req {id}: {error}");
    }
    let _ = tx.send(payload);
}

/// Determine the brain directory relative to the project root.
pub fn find_brain_dir(project_root: &Path) -> PathBuf {
    project_root.join("brain")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A fake brain: a shell loop speaking just enough JSON-RPC to test the
    /// client's supervision behavior.
    ///
    /// * `boom` — exit without replying, but only for the first process that
    ///   sees it (a marker file survives the restart), so a retry can succeed.
    /// * `hang` — read the request and never reply.
    /// * `err`  — reply with a JSON-RPC error object.
    /// * anything else (including `ping`) — reply with a success result.
    fn fake_brain(marker: &Path) -> BrainSpawnConfig {
        let script = format!(
            r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":[[:space:]]*\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"boom"'*)
      if [ ! -f "{marker}" ]; then : > "{marker}"; exit 0; fi ;;
    *'"method":"hang"'*)
      continue ;;
    *'"method":"err"'*)
      printf '{{"jsonrpc":"2.0","error":{{"code":-32000,"message":"kaboom"}},"id":%s}}\n' "$id"
      continue ;;
    *'"method":"noisy"'*)
      printf '{{"jsonrpc":"2.0","method":"notify","params":{{"message":"hi"}}}}\n' ;;
  esac
  printf '{{"jsonrpc":"2.0","result":{{"status":"ok","echo":%s}},"id":%s}}\n' "$id" "$id"
done
"#,
            marker = marker.display()
        );

        BrainSpawnConfig {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script],
            working_dir: std::env::temp_dir(),
            env: Vec::new(),
            restart_max: 0,
            restart_window: Duration::from_secs(600),
            restart_backoff: Duration::from_millis(10),
            call_timeout: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    async fn round_trips_a_request() {
        let dir = TempDir::new().unwrap();
        let client = BrainClient::new(fake_brain(&dir.path().join("marker")));
        client.start().await.unwrap();

        let result = client.call("route", serde_json::json!({})).await.unwrap();
        assert_eq!(result["status"], "ok");
        assert!(client.is_running().await);

        client.stop().await.unwrap();
    }

    #[tokio::test]
    async fn notifications_do_not_break_response_matching() {
        let dir = TempDir::new().unwrap();
        let client = BrainClient::new(fake_brain(&dir.path().join("marker")));
        client.start().await.unwrap();

        let result = client.call("noisy", serde_json::json!({})).await.unwrap();
        assert_eq!(result["status"], "ok");

        client.stop().await.unwrap();
    }

    #[tokio::test]
    async fn death_mid_call_fails_the_caller_instead_of_hanging() {
        let dir = TempDir::new().unwrap();
        // restart_max = 0, so the failure surfaces rather than being retried.
        let client = BrainClient::new(fake_brain(&dir.path().join("marker")));
        client.start().await.unwrap();

        // Previously this awaited a oneshot nobody would ever complete.
        let err = tokio::time::timeout(
            Duration::from_secs(10),
            client.call("boom", serde_json::json!({})),
        )
        .await
        .expect("call must not hang when the brain dies")
        .expect_err("a dead brain cannot answer");

        assert!(
            matches!(err, BrainError::RestartsExhausted(0)),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn written_request_is_not_resent_after_a_crash() {
        let dir = TempDir::new().unwrap();
        let config = fake_brain(&dir.path().join("marker")).with_restart_max(2);
        let client = BrainClient::new(config);
        client.start().await.unwrap();

        // The first process dies on `boom` after reading it. The request was
        // written, so it must NOT be replayed on the replacement (the marker
        // file would let a replay succeed).
        let err = client
            .call("boom", serde_json::json!({}))
            .await
            .expect_err("a written request is not retried");
        assert!(matches!(err, BrainError::Transport(_)), "got {err}");
        // The brain was replaced for the next caller.
        assert_eq!(client.restarts_used(), 1);
        assert!(client.is_running().await);
        let ok = client.call("route", serde_json::json!({})).await.unwrap();
        assert_eq!(ok["status"], "ok");

        client.stop().await.unwrap();
    }

    #[tokio::test]
    async fn unsent_request_is_retried_after_restart() {
        let dir = TempDir::new().unwrap();
        let config = fake_brain(&dir.path().join("marker")).with_restart_max(2);
        let client = BrainClient::new(config);
        client.start().await.unwrap();

        // Take the brain down between calls: the next request never reaches it.
        client.terminate().await;
        let result = client.call("route", serde_json::json!({})).await.unwrap();
        assert_eq!(result["status"], "ok");
        assert_eq!(client.restarts_used(), 1);

        client.stop().await.unwrap();
    }

    #[tokio::test]
    async fn restart_counter_resets_after_a_healthy_window() {
        let dir = TempDir::new().unwrap();
        let config = fake_brain(&dir.path().join("marker"))
            .with_restart_max(1)
            .with_restart_window(Duration::from_millis(150));
        let client = BrainClient::new(config);
        client.start().await.unwrap();

        client
            .call("boom", serde_json::json!({}))
            .await
            .unwrap_err();
        assert_eq!(client.restarts_used(), 1);

        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(client.restarts_used(), 0, "window elapsed");

        std::fs::remove_file(dir.path().join("marker")).unwrap();
        client
            .call("boom", serde_json::json!({}))
            .await
            .unwrap_err();
        // Budget of 1 was available again, so the brain is back up.
        assert!(client.is_running().await);

        client.stop().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stopping_kills_grandchildren_in_the_process_group() {
        let dir = TempDir::new().unwrap();
        let pid_file = dir.path().join("grandchild.pid");
        let script = format!(
            r#"
sleep 4321 &
echo $! > "{pid}"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":[[:space:]]*\([0-9]*\).*/\1/p')
  printf '{{"jsonrpc":"2.0","result":{{"status":"ok"}},"id":%s}}\n' "$id"
done
"#,
            pid = pid_file.display()
        );
        let config = BrainSpawnConfig {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script],
            working_dir: std::env::temp_dir(),
            env: Vec::new(),
            restart_max: 0,
            restart_window: Duration::from_secs(600),
            restart_backoff: Duration::from_millis(10),
            call_timeout: Duration::from_secs(5),
        };
        let client = BrainClient::new(config);
        client.start().await.unwrap();

        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "grandchild is alive");

        client.stop().await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "grandchild must be killed with the group"
        );
    }

    #[tokio::test]
    async fn restart_budget_is_finite() {
        let dir = TempDir::new().unwrap();
        let config = fake_brain(&dir.path().join("marker")).with_restart_max(1);
        let client = BrainClient::new(config);
        client.start().await.unwrap();

        // Burn the single restart.
        client
            .call("boom", serde_json::json!({}))
            .await
            .unwrap_err();
        assert_eq!(client.restarts_used(), 1);

        // Kill it again by removing the marker, so `boom` is fatal once more.
        std::fs::remove_file(dir.path().join("marker")).unwrap();
        let err = client
            .call("boom", serde_json::json!({}))
            .await
            .expect_err("no restarts left");
        assert!(
            matches!(err, BrainError::RestartsExhausted(1)),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn a_wedged_brain_times_out_and_is_replaced() {
        let dir = TempDir::new().unwrap();
        let config = fake_brain(&dir.path().join("marker"))
            .with_call_timeout(Duration::from_millis(200))
            .with_restart_max(3);
        let client = BrainClient::new(config);
        client.start().await.unwrap();

        let err = client
            .call("hang", serde_json::json!({}))
            .await
            .expect_err("a silent brain must time out");
        assert!(
            matches!(err, BrainError::Timeout { .. }),
            "unexpected error: {err}"
        );
        // A timeout must not re-send a possibly side-effecting request, but
        // the wedged brain is replaced so the next call does not wait again.
        assert_eq!(client.restarts_used(), 1);
        let ok = client.call("route", serde_json::json!({})).await.unwrap();
        assert_eq!(ok["status"], "ok");

        client.stop().await.unwrap();
    }

    #[tokio::test]
    async fn rpc_errors_are_reported_without_restarting() {
        let dir = TempDir::new().unwrap();
        let config = fake_brain(&dir.path().join("marker")).with_restart_max(3);
        let client = BrainClient::new(config);
        client.start().await.unwrap();

        let err = client
            .call("err", serde_json::json!({}))
            .await
            .expect_err("the brain reported a failure");
        assert!(matches!(err, BrainError::Rpc(_)), "unexpected error: {err}");
        assert!(err.to_string().contains("kaboom"));
        assert_eq!(client.restarts_used(), 0, "an RPC error is not a crash");

        client.stop().await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_calls_are_multiplexed_by_id() {
        let dir = TempDir::new().unwrap();
        let client = Arc::new(BrainClient::new(fake_brain(&dir.path().join("marker"))));
        client.start().await.unwrap();

        let calls = (0..16).map(|_| {
            let client = client.clone();
            tokio::spawn(async move { client.call("route", serde_json::json!({})).await })
        });

        let mut ids = Vec::new();
        for call in calls {
            let value = call.await.unwrap().unwrap();
            ids.push(value["echo"].as_u64().unwrap());
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 16, "every caller got its own response");

        client.stop().await.unwrap();
    }
}
