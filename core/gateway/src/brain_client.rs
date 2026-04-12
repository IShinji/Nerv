use anyhow::{Context, Result};
use nerv_shared::ipc::{JsonRpcRequest, JsonRpcResponse};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, info, warn};

const BRAIN_READY_TIMEOUT_SECS: u64 = 600;

/// Manages the Python brain subprocess and JSON-RPC communication.
pub struct BrainClient {
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    pending_requests: Arc<StdMutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    next_id: AtomicU64,
    python_command: String,
    brain_module: String,
    brain_dir: String,
    router_model: String,
    router_url: String,
    tier0_model: String,
    tier1_model: String,
    tier2_model: String,
    tier3_model: String,
    #[allow(dead_code)]
    restart_max: u32,
}

impl BrainClient {
    pub fn new(
        python_command: &str,
        brain_module: &str,
        brain_dir: &str,
        router_model: &str,
        router_url: &str,
        tier0_model: &str,
        tier1_model: &str,
        tier2_model: &str,
        tier3_model: &str,
        restart_max: u32,
    ) -> Self {
        Self {
            child: Mutex::new(None),
            stdin: Mutex::new(None),
            pending_requests: Arc::new(StdMutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            python_command: python_command.to_string(),
            brain_module: brain_module.to_string(),
            brain_dir: brain_dir.to_string(),
            router_model: router_model.to_string(),
            router_url: router_url.to_string(),
            tier0_model: tier0_model.to_string(),
            tier1_model: tier1_model.to_string(),
            tier2_model: tier2_model.to_string(),
            tier3_model: tier3_model.to_string(),
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
        let (program, extra_args) = parts.split_first().context("python_command is empty")?;
        let uv_cache_dir = Path::new(&self.brain_dir).join(".uv-cache");

        let mut cmd = Command::new(program);
        cmd.args(extra_args)
            .arg("-m")
            .arg(&self.brain_module)
            .current_dir(&self.brain_dir)
            .env("NERV_ROUTER_MODEL", &self.router_model)
            .env("NERV_OLLAMA_URL", &self.router_url)
            .env("NERV_MODEL_TIER0", &self.tier0_model)
            .env("NERV_MODEL_TIER1", &self.tier1_model)
            .env("NERV_MODEL_TIER2", &self.tier2_model)
            .env("NERV_MODEL_TIER3", &self.tier3_model)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit()); // stderr → tracing output

        if std::env::var_os("UV_CACHE_DIR").is_none() {
            cmd.env("UV_CACHE_DIR", uv_cache_dir);
        }

        let mut child = cmd.spawn().context("failed to spawn brain process")?;

        let stdin = child
            .stdin
            .take()
            .context("failed to capture brain stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("failed to capture brain stdout")?;

        *self.child.lock().await = Some(child);
        *self.stdin.lock().await = Some(stdin);

        let pending_requests = Arc::clone(&self.pending_requests);
        let mut reader = BufReader::new(stdout);
        tokio::spawn(async move {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        debug!("Brain process stdout closed");
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        
                        // Parse as generic JSON to determine if it's a response or notification
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
                            if let Some(method) = value.get("method").and_then(|m| m.as_str()) {
                                if method == "notify" {
                                    if let Some(params) = value.get("params") {
                                        let msg = params.get("message").and_then(|m| m.as_str()).unwrap_or(trimmed);
                                        info!("⚡[Brain]: {}", msg);
                                    } else {
                                        info!("⚡[Brain]: {}", trimmed);
                                    }
                                    continue;
                                }
                            }
                            
                            // Otherwise, treat as a JsonRpcResponse
                            if let Ok(response) = serde_json::from_value::<JsonRpcResponse>(value.clone()) {
                                let id = response.id;
                                let mut map = pending_requests.lock().unwrap();
                                if let Some(tx) = map.remove(&id) {
                                    // Parse inner result based on our defined response format
                                    let res = match response.into_result() {
                                        Ok(r) => r,
                                        Err(e) => {
                                            warn!("Brain returned error for req {}: {}", id, e);
                                            // Sending default/error value backward so client doesn't freeze forever
                                            serde_json::json!({"error": e.to_string()})
                                        }
                                    };
                                    let _ = tx.send(res);
                                } else {
                                    debug!("Received response for unknown/expired request id: {}", id);
                                }
                            } else {
                                // Fallback info printer
                                info!("⚡ {}", trimmed);
                            }
                        } else {
                            debug!("← Brain (raw): {}", trimmed);
                        }
                    }
                    Err(e) => {
                        warn!("Error reading from brain stdout: {}", e);
                        break;
                    }
                }
            }
        });

        // Give the subprocess a moment to fail fast on missing deps/config.
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        if let Some(child) = self.child.lock().await.as_mut() {
            if let Some(status) = child
                .try_wait()
                .context("failed to check brain process status")?
            {
                anyhow::bail!("brain process exited during startup: {status}");
            }
        }

        info!("Waiting for brain process readiness...");
        let readiness = tokio::time::timeout(
            Duration::from_secs(BRAIN_READY_TIMEOUT_SECS),
            self.call("ping", serde_json::json!({})),
        )
        .await
        .context("timed out waiting for brain process readiness")??;

        if readiness.get("status").and_then(|value| value.as_str()) != Some("ok") {
            anyhow::bail!("brain process returned unexpected readiness response: {readiness}");
        }

        info!("Brain process started successfully");
        Ok(())
    }

    /// Send a JSON-RPC request and wait for the response.
    pub async fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = JsonRpcRequest::new(method, params, id);
        let line = request.to_line()?;

        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.pending_requests.lock().unwrap();
            map.insert(id, tx);
        }

        debug!("→ Brain: {}", line.trim());

        // Send request
        {
            let mut stdin_lock = self.stdin.lock().await;
            let stdin = stdin_lock.as_mut().context("brain process not started")?;
            stdin
                .write_all(line.as_bytes())
                .await
                .context("failed to write to brain stdin")?;
            stdin.flush().await.context("failed to flush brain stdin")?;
        }

        // Wait for response via oneshot channel asynchronously
        let result = rx
            .await
            .context(format!("brain process closed before responding to request id {}", id))?;
            
        if result.get("error").is_some() {
            anyhow::bail!("Brain returned error: {}", result);
        }

        Ok(result)
    }

    /// Stop the brain subprocess gracefully.
    pub async fn stop(&self) -> Result<()> {
        // Drop stdin to signal EOF to the Python process
        self.stdin.lock().await.take();

        if let Some(mut child) = self.child.lock().await.take() {
            info!("Waiting for brain process to exit...");
            match tokio::time::timeout(tokio::time::Duration::from_secs(5), child.wait()).await {
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
    project_root.join("brain").to_string_lossy().into_owned()
}
