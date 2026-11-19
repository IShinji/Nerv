//! Claude Code CLI backend — runs prompts through the local `claude` binary.
//!
//! Port of `brain/nerv/llm/claude_cli.py`: subscription auth, print mode,
//! isolation flags, failure classification, the retry loop and the MCP tool
//! bridge. Claude Code runs its own agentic tool loop (Nerv tools exposed via
//! MCP), so one call returns the final answer and `runs_own_tool_loop` is set
//! to tell the caller not to loop again.
//!
//! This uses the user's logged-in Claude Code session (subscription auth), so
//! no ANTHROPIC_API_KEY is required.

mod classify;
mod invoke;
mod mcp;

#[cfg(test)]
mod tests;

pub use classify::{backoff_seconds, classify_failure, ClaudeCliError, FailureKind};
pub use invoke::flatten_messages;
pub use mcp::{resolve_provider_names, write_mcp_config, MCP_CONFIG_PREFIX};

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tracing::warn;

use crate::llm::{ChatMessage, CompletionResult, LlmError};

/// Errors we cannot classify are retried once (the pre-classification default),
/// mirroring `UNKNOWN_ERROR_MAX_ATTEMPTS` in Python.
pub const UNKNOWN_ERROR_MAX_ATTEMPTS: u32 = 2;

/// Flags that isolate `claude -p` from the user's personal Claude Code setup
/// (global CLAUDE.md, hooks, plugins, MCP servers, skills, built-in tools).
/// `--tools ""` removes every built-in tool.
const ISOLATION_ARGS: &[&str] = &[
    "--tools",
    "",
    "--setting-sources",
    "",
    "--strict-mcp-config",
    "--no-session-persistence",
    "--disable-slash-commands",
];

/// Everything one claude-cli turn needs beyond the backend parameters.
#[derive(Debug, Clone)]
pub struct ClaudeCliRequest<'a> {
    pub model_name: &'a str,
    pub json_mode: bool,
    /// Native tool/capability names to expose to Claude via MCP.
    pub tool_names: &'a [String],
    /// Required for the MCP bridge; None means no tools can run.
    pub project_root: Option<&'a Path>,
    /// The conversation scope for this request (the MCP server's memory
    /// tools read it from NERV_MEMORY_SCOPE).
    pub scope: &'a str,
}

/// Backend parameters, injectable for tests (fake `claude` executable, zero
/// backoff) so tests never race on process environment variables.
#[derive(Clone)]
pub struct ClaudeCliBackend {
    pub bin: String,
    pub max_attempts: u32,
    /// Backoff seconds before the next attempt; swappable in tests.
    pub backoff: fn(u32) -> f64,
}

impl Default for ClaudeCliBackend {
    fn default() -> Self {
        Self {
            bin: std::env::var("NERV_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string()),
            max_attempts: std::env::var("NERV_CLAUDE_MAX_ATTEMPTS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2)
                .max(1),
            backoff: classify::backoff_seconds,
        }
    }
}

impl std::fmt::Debug for ClaudeCliBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeCliBackend")
            .field("bin", &self.bin)
            .field("max_attempts", &self.max_attempts)
            .finish()
    }
}

/// Run one turn through the `claude` CLI in print mode (default backend).
pub async fn complete(
    messages: &[ChatMessage],
    request: &ClaudeCliRequest<'_>,
    timeout: Duration,
) -> Result<CompletionResult, LlmError> {
    complete_with(&ClaudeCliBackend::default(), messages, request, timeout).await
}

/// Run one turn with an explicit backend (tests inject a fake `claude`).
pub async fn complete_with(
    backend: &ClaudeCliBackend,
    messages: &[ChatMessage],
    request: &ClaudeCliRequest<'_>,
    timeout: Duration,
) -> Result<CompletionResult, LlmError> {
    let (system_prompt, conversation) = invoke::flatten_messages(messages);
    let mut system_prompt = system_prompt;
    if request.json_mode {
        system_prompt = format!(
            "{system_prompt}\n\nRespond with ONLY a single valid JSON object and \
             no other text."
        )
        .trim()
        .to_string();
    }

    let mut args: Vec<String> = vec![
        "-p".to_string(),
        "--model".to_string(),
        if request.model_name.is_empty() {
            "opus"
        } else {
            request.model_name
        }
        .to_string(),
        "--output-format".to_string(),
        "json".to_string(),
    ];
    args.extend(ISOLATION_ARGS.iter().map(|s| s.to_string()));
    if !system_prompt.is_empty() {
        args.push("--system-prompt".to_string());
        args.push(system_prompt);
    }

    let provider_names = mcp::resolve_provider_names(request.tool_names);
    let mcp_config_path = match (request.project_root, provider_names.is_empty()) {
        (Some(project_root), false) => Some(
            mcp::write_mcp_config(project_root, request.scope).map_err(|e| {
                LlmError::ClaudeCli(ClaudeCliError {
                    kind: FailureKind::Fatal,
                    message: format!("failed to write the MCP config: {e}"),
                })
            })?,
        ),
        _ => None,
    };
    if let Some(path) = &mcp_config_path {
        args.push("--mcp-config".to_string());
        args.push(path.to_string_lossy().to_string());
        args.push("--allowedTools".to_string());
        // Namespaced per server: every tool runs on the native rust server.
        for name in &provider_names {
            args.push(format!("mcp__{}__{name}", mcp::MCP_SERVER_NAME));
        }
    }

    let mut attempts = backend.max_attempts.max(1);
    let mut last_error = "unknown error".to_string();
    let mut attempt = 0u32;
    let outcome = loop {
        if attempt >= attempts {
            break Err(LlmError::ClaudeCli(ClaudeCliError {
                kind: FailureKind::Unknown,
                message: format!("claude CLI failed after {attempts} attempt(s): {last_error}"),
            }));
        }
        attempt += 1;

        match invoke::invoke_once(&backend.bin, &args, &conversation, timeout).await {
            Err(e) => {
                last_error = e.message.clone();
                warn!(
                    "claude CLI attempt {attempt}/{attempts} failed ({:?}): {e}",
                    e.kind
                );
                match e.kind {
                    FailureKind::Fatal => break Err(e.into()),
                    FailureKind::Unknown => attempts = attempts.min(UNKNOWN_ERROR_MAX_ATTEMPTS),
                    FailureKind::Retry => {}
                }
                if attempt < attempts {
                    sleep_between_attempts(backend, attempt).await;
                }
            }
            Ok(data) => {
                if is_truthy(data.get("is_error")) {
                    let status = data.get("api_error_status").and_then(|v| v.as_i64());
                    let error_type = data
                        .get("api_error_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    // Python: str(data.get("result") or data.get("subtype") or "")
                    let text = ["result", "subtype"]
                        .iter()
                        .find_map(|k| {
                            data.get(*k)
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                        })
                        .unwrap_or("");
                    let (kind, message) = classify::classify_failure(status, error_type, text);
                    if kind == FailureKind::Fatal {
                        break Err(LlmError::ClaudeCli(ClaudeCliError { kind, message }));
                    }
                    if kind == FailureKind::Unknown {
                        attempts = attempts.min(UNKNOWN_ERROR_MAX_ATTEMPTS);
                    }
                    last_error = format!("claude returned error result: {message}");
                    warn!(
                        "claude CLI attempt {attempt}/{attempts} returned an error \
                         result ({kind:?})"
                    );
                    if attempt < attempts {
                        sleep_between_attempts(backend, attempt).await;
                    }
                } else {
                    // Claude Code already executed any tool calls internally via
                    // MCP, so the caller must not run its own tool loop on top
                    // of this.
                    break Ok(CompletionResult {
                        content: data
                            .get("result")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        tool_calls: Vec::new(),
                        runs_own_tool_loop: true,
                        provider: "claude-cli".to_string(),
                        duration_ms: data.get("duration_ms").and_then(|v| v.as_f64()),
                        cost_usd: data.get("total_cost_usd").and_then(|v| v.as_f64()),
                        usage: data.get("usage").cloned(),
                    });
                }
            }
        }
    };

    // Remove the temp MCP config whatever the outcome (the Python `finally`).
    if let Some(path) = &mcp_config_path {
        let _ = std::fs::remove_file(path);
    }
    outcome
}

async fn sleep_between_attempts(backend: &ClaudeCliBackend, attempt: u32) {
    let secs = (backend.backoff)(attempt);
    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
}

/// Python truthiness for the JSON `is_error` field (bool, non-zero number or
/// non-empty string are all truthy).
pub(crate) fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        _ => false,
    }
}
