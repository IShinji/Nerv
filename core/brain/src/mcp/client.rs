//! Minimal MCP stdio client implementation.
//!
//! Port of `brain/nerv/mcp/client.py`: one JSON-RPC session over a child
//! process's stdio, with per-request timeouts and scrubbed env inheritance.
//!
//! ## Security note on process spawning
//!
//! The server command comes from the *operator's* config file (the same
//! trust level as the gateway's own launch configuration). It is exec'd in
//! arg-list form — never through a shell — and is validated to contain no
//! shell metacharacters, so nothing is ever interpolated into a shell
//! command line.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::tools::sandbox::scrubbed_env;

pub const DEFAULT_MCP_PROTOCOL_VERSION: &str = "2024-11-05";
pub const DEFAULT_MCP_REQUEST_TIMEOUT: f64 = 30.0;

/// Errors from the MCP client.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("MCP transport failure: {0}")]
    Transport(String),
    #[error("MCP protocol failure: {0}")]
    Protocol(String),
}

/// Reject anything that could alter how a shell would parse the command.
/// The command is exec'd directly in arg-list form (never a shell), so this
/// is defense in depth — but it also keeps an operator typo from becoming a
/// shell one-liner. Arguments belong in the config's `args` list.
pub fn validate_command(command: &str) -> Result<(), McpError> {
    const FORBIDDEN: [char; 16] = [
        ';', '&', '|', '`', '$', '(', ')', '<', '>', '\n', '\r', '\t', '\\', '\'', '"', '!',
    ];
    if command.is_empty() {
        return Err(McpError::Transport(
            "MCP server command is empty.".to_string(),
        ));
    }
    if let Some(ch) = command.chars().find(|c| FORBIDDEN.contains(c)) {
        return Err(McpError::Transport(format!(
            "MCP server command contains the shell metacharacter {ch:?}; \
             pass arguments as the config's `args` list instead."
        )));
    }
    Ok(())
}

/// Runtime configuration for an MCP server process.
#[derive(Debug, Clone, Default)]
pub struct McpServerConfig {
    pub name: String,
    pub preset: String,
    pub enabled: bool,
    pub transport: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: String,
    pub capabilities: Vec<String>,
    pub action_map: HashMap<String, String>,
    /// Seconds to wait for any single request (initialize, list, call).
    pub request_timeout: f64,
}

impl McpServerConfig {
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs_f64(if self.request_timeout > 0.0 {
            self.request_timeout
        } else {
            DEFAULT_MCP_REQUEST_TIMEOUT
        })
    }
}

/// Shared JSON-RPC session state: the child's stdin plus the pending map.
struct SharedSession {
    stdin: Mutex<tokio::process::ChildStdin>,
    pending: std::sync::Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Result<Value, McpError>>>>,
    next_id: std::sync::atomic::AtomicI64,
    closed: std::sync::atomic::AtomicBool,
}

fn fail_pending(session: &SharedSession, error: &McpError) {
    let mut pending = session.pending.lock().unwrap();
    for (_, sender) in pending.drain() {
        let _ = sender.send(Err(McpError::Transport(error.to_string())));
    }
}

/// Route JSON-RPC responses from the server's stdout to their waiters.
async fn read_stdout_loop<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    session: Arc<SharedSession>,
    server_name: String,
) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    warn!("Invalid MCP JSON from {server_name}");
                    continue;
                };
                let Some(id) = message.get("id").and_then(|v| v.as_i64()) else {
                    debug!("Ignoring MCP notification from {server_name}");
                    continue;
                };
                let sender = session.pending.lock().unwrap().remove(&id);
                if let Some(sender) = sender {
                    if let Some(error) = message.get("error") {
                        let text = error
                            .get("message")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| error.to_string());
                        let _ = sender.send(Err(McpError::Protocol(format!(
                            "MCP server {server_name} error: {text}"
                        ))));
                    } else {
                        let _ =
                            sender.send(Ok(message.get("result").cloned().unwrap_or(json!({}))));
                    }
                }
            }
            Ok(None) => break, // EOF
            Err(_) => break,
        }
    }
    // Reader is gone: nobody will answer the pending requests.
    fail_pending(
        &session,
        &McpError::Transport(format!("MCP server {server_name} closed its output.")),
    );
}

/// A small JSON-RPC client for MCP stdio servers.
pub struct StdioMcpClient {
    config: McpServerConfig,
    project_root: PathBuf,
    protocol_version: String,
    session: Mutex<Option<Arc<SharedSession>>>,
    initialized: bool,
}

impl StdioMcpClient {
    pub fn new(config: McpServerConfig, project_root: &Path, protocol_version: &str) -> Self {
        Self {
            config,
            project_root: project_root.to_path_buf(),
            protocol_version: protocol_version.to_string(),
            session: Mutex::new(None),
            initialized: false,
        }
    }

    /// Start the server process and complete MCP initialization.
    pub async fn connect(&mut self) -> Result<(), McpError> {
        if self.initialized {
            return Ok(());
        }
        if self.config.transport != "stdio" {
            return Err(McpError::Transport(format!(
                "Unsupported MCP transport for {}: {}",
                self.config.name, self.config.transport
            )));
        }
        if self.config.command.trim().is_empty() {
            return Err(McpError::Transport(format!(
                "MCP server {} is enabled but has no command configured.",
                self.config.name
            )));
        }
        validate_command(&self.config.command)?;

        // Explicit per-server env is the supported way to hand a server its
        // credentials; ambient credential-shaped names are not inherited.
        let mut env = scrubbed_env(&[]);
        env.extend(self.config.env.iter().map(|(k, v)| (k.clone(), v.clone())));

        let cwd = if self.config.cwd.trim().is_empty() {
            self.project_root.clone()
        } else {
            let candidate = PathBuf::from(self.config.cwd.trim());
            if candidate.is_absolute() {
                candidate
            } else {
                self.project_root.join(candidate)
            }
        };

        // Arg-list exec (never a shell): argv[0] is the operator-configured
        // program name, each remaining element stays its own argv entry.
        let mut argv: Vec<String> = Vec::with_capacity(self.config.args.len() + 1);
        argv.push(self.config.command.trim().to_string());
        argv.extend(self.config.args.iter().cloned());
        let mut child = tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&cwd)
            .envs(env)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                McpError::Transport(format!(
                    "Failed to start MCP server {}: {e}",
                    self.config.name
                ))
            })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            McpError::Transport(format!("MCP server {} stdin unavailable", self.config.name))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            McpError::Transport(format!(
                "MCP server {} stdout unavailable",
                self.config.name
            ))
        })?;
        let stderr = child.stderr.take();

        let session = Arc::new(SharedSession {
            stdin: Mutex::new(stdin),
            pending: std::sync::Mutex::new(HashMap::new()),
            next_id: std::sync::atomic::AtomicI64::new(0),
            closed: std::sync::atomic::AtomicBool::new(false),
        });
        *self.session.lock().await = Some(Arc::clone(&session));

        tokio::spawn(read_stdout_loop(
            stdout,
            Arc::clone(&session),
            self.config.name.clone(),
        ));
        if let Some(stderr) = stderr {
            let name = self.config.name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    debug!("[mcp:{name} stderr] {line}");
                }
            });
        }
        // Reap the child so no zombie is left behind.
        tokio::spawn(async move {
            let mut child: Child = child;
            let _ = child.wait().await;
        });

        self.send_request(
            "initialize",
            json!({
                "protocolVersion": self.protocol_version,
                "capabilities": {},
                "clientInfo": {"name": "nerv", "version": "0.1.0"},
            }),
        )
        .await?;
        self.send_notification("notifications/initialized", json!({}))
            .await?;
        self.initialized = true;
        Ok(())
    }

    /// Fetch the currently available tool definitions from the server.
    pub async fn list_tools(&mut self) -> Result<Vec<Value>, McpError> {
        self.connect().await?;
        let result = self.send_request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(|v| v.as_array())
            .cloned()
            .ok_or_else(|| {
                McpError::Protocol(format!(
                    "MCP server {} returned invalid tools/list result.",
                    self.config.name
                ))
            })?;
        Ok(tools)
    }

    /// The tool names the server exposes (sorted, deduped).
    pub async fn tool_names(&mut self) -> Result<Vec<String>, McpError> {
        let tools = self.list_tools().await?;
        let mut names: Vec<String> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
            .filter(|n| !n.trim().is_empty())
            .map(|n| n.trim().to_string())
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    /// Invoke a tool on the connected MCP server.
    pub async fn call_tool(&mut self, name: &str, arguments: &Value) -> Result<Value, McpError> {
        self.connect().await?;
        self.send_request("tools/call", json!({"name": name, "arguments": arguments}))
            .await
    }

    /// Terminate the child process and drop the session.
    pub async fn close(&mut self) {
        if let Some(session) = self.session.lock().await.take() {
            session
                .closed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            fail_pending(&session, &McpError::Transport("client closed".to_string()));
        }
        self.initialized = false;
        // The spawned reaper task holds the Child; kill_on_drop reaps it.
    }

    async fn send_request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let session = self.session.lock().await.clone().ok_or_else(|| {
            McpError::Transport(format!("MCP server {} is not running.", self.config.name))
        })?;
        if session.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(McpError::Transport(format!(
                "MCP server {} is not running.",
                self.config.name
            )));
        }

        let request_id = session
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let (tx, rx) = tokio::sync::oneshot::channel();
        session.pending.lock().unwrap().insert(request_id, tx);

        let payload = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        });
        {
            let mut stdin = session.stdin.lock().await;
            if let Err(e) = stdin
                .write_all((serde_json::to_string(&payload).unwrap_or_default() + "\n").as_bytes())
                .await
            {
                session.pending.lock().unwrap().remove(&request_id);
                return Err(McpError::Transport(format!(
                    "Failed to write request to MCP server {}: {e}",
                    self.config.name
                )));
            }
            let _ = stdin.flush().await;
        }

        let timeout = self.config.request_timeout();
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(McpError::Transport(format!(
                "MCP server {} closed its output.",
                self.config.name
            ))),
            Err(_) => {
                session.pending.lock().unwrap().remove(&request_id);
                Err(McpError::Transport(format!(
                    "MCP server {} timed out after {}s handling {method}. \
                     Raise request_timeout for this server in the config if it is just slow.",
                    self.config.name, self.config.request_timeout
                )))
            }
        }
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), McpError> {
        let session = self.session.lock().await.clone().ok_or_else(|| {
            McpError::Transport(format!("MCP server {} is not running.", self.config.name))
        })?;
        let payload = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let mut stdin = session.stdin.lock().await;
        stdin
            .write_all((serde_json::to_string(&payload).unwrap_or_default() + "\n").as_bytes())
            .await
            .map_err(|e| {
                McpError::Transport(format!(
                    "Failed to write to MCP server {}: {e}",
                    self.config.name
                ))
            })?;
        let _ = stdin.flush().await;
        Ok(())
    }
}
