use anyhow::{Context, Result};
use nerv_shared::ipc::{JsonRpcRequest, JsonRpcResponse};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Manages the Python brain subprocess and JSON-RPC communication.
pub struct BrainClient {
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    stdout: Mutex<Option<BufReader<tokio::process::ChildStdout>>>,
    next_id: AtomicU64,
    python_command: String,
    brain_module: String,
    brain_dir: String,
    #[allow(dead_code)]
    restart_max: u32,
}

impl BrainClient {
    pub fn new(
        python_command: &str,
        brain_module: &str,
        brain_dir: &str,
        restart_max: u32,
    ) -> Self {
        Self {
            child: Mutex::new(None),
            stdin: Mutex::new(None),
            stdout: Mutex::new(None),
            next_id: AtomicU64::new(1),
            python_command: python_command.to_string(),
            brain_module: brain_module.to_string(),
            brain_dir: brain_dir.to_string(),
            restart_max,
        }
    }

    /// Start the Python brain subprocess.
    pub async fn start(&self) -> Result<()> {
        info!(
            "Starting brain process: {} -m {} (cwd: {})",
            self.python_command, self.brain_module, self.brain_dir
        );

        // Support multi-word commands like "uv run python"
        let parts: Vec<&str> = self.python_command.split_whitespace().collect();
        let (program, extra_args) = parts
            .split_first()
            .context("python_command is empty")?;

        let mut cmd = Command::new(program);
        cmd.args(extra_args)
            .arg("-m")
            .arg(&self.brain_module)
            .current_dir(&self.brain_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit()); // stderr → tracing output

        let mut child = cmd.spawn().context("failed to spawn brain process")?;

        let stdin = child.stdin.take().context("failed to capture brain stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("failed to capture brain stdout")?;

        *self.child.lock().await = Some(child);
        *self.stdin.lock().await = Some(stdin);
        *self.stdout.lock().await = Some(BufReader::new(stdout));

        // Wait briefly and check if the process crashed immediately
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        info!("Brain process started successfully");
        Ok(())
    }

    /// Send a JSON-RPC request and wait for the response.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = JsonRpcRequest::new(method, params, id);
        let line = request.to_line()?;

        debug!("→ Brain: {}", line.trim());

        // Send request
        {
            let mut stdin_lock = self.stdin.lock().await;
            let stdin = stdin_lock
                .as_mut()
                .context("brain process not started")?;
            stdin
                .write_all(line.as_bytes())
                .await
                .context("failed to write to brain stdin")?;
            stdin
                .flush()
                .await
                .context("failed to flush brain stdin")?;
        }

        // Read response
        let mut response_line = String::new();
        {
            let mut stdout_lock = self.stdout.lock().await;
            let stdout = stdout_lock
                .as_mut()
                .context("brain process not started")?;
            stdout
                .read_line(&mut response_line)
                .await
                .context("failed to read from brain stdout")?;
        }

        if response_line.is_empty() {
            anyhow::bail!("brain process closed stdout unexpectedly");
        }

        debug!("← Brain: {}", response_line.trim());

        let response = JsonRpcResponse::from_line(&response_line)?;
        let result = response.into_result()?;
        Ok(result)
    }

    /// Stop the brain subprocess gracefully.
    pub async fn stop(&self) -> Result<()> {
        // Drop stdin to signal EOF to the Python process
        self.stdin.lock().await.take();

        if let Some(mut child) = self.child.lock().await.take() {
            info!("Waiting for brain process to exit...");
            match tokio::time::timeout(
                tokio::time::Duration::from_secs(5),
                child.wait(),
            )
            .await
            {
                Ok(Ok(status)) => info!("Brain process exited: {status}"),
                Ok(Err(e)) => warn!("Error waiting for brain process: {e}"),
                Err(_) => {
                    warn!("Brain process did not exit in time, killing");
                    child.kill().await.ok();
                }
            }
        }

        Ok(())
    }

    /// Check if the process is running by checking its existence.
    #[allow(dead_code)]
    pub async fn is_running(&self) -> bool {
        if let Some(child) = self.child.lock().await.as_mut() {
            // try_wait returns Ok(None) if the process is still running
            matches!(child.try_wait(), Ok(None))
        } else {
            false
        }
    }
}

/// Determine the brain directory relative to the project root.
pub fn find_brain_dir(project_root: &Path) -> String {
    project_root
        .join("brain")
        .to_string_lossy()
        .into_owned()
}
