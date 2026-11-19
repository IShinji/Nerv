//! Unified LLM client — one model call, dispatched by model-string prefix.
//!
//! Port of `brain/nerv/llm/` (client.py + claude_cli.py + the ollama half).
//! `complete()` performs exactly ONE assistant turn and returns its content
//! plus any tool calls. The orchestrator owns the tool-execution loop for the
//! ollama/litellm providers; the claude-cli provider runs Claude Code's own
//! agentic loop internally (tools exposed via MCP), so it returns the final
//! answer and sets `runs_own_tool_loop` to tell the caller not to loop again.
//!
//! Model string scheme (the single switch point for swapping providers):
//!   "claude-cli:<alias>"   -> local Claude Code CLI (uses your subscription)
//!   "local:<ollama-model>" -> local Ollama HTTP (reserved; dormant)
//!   "litellm:<model>"      -> litellm cloud API (reserved, not ported yet —
//!                             there is no litellm library in Rust; when a
//!                             caller needs it this becomes a thin
//!                             OpenAI-compatible client per vendor)
//!   bare cloud name        -> treated as litellm

pub mod claude_cli;
pub mod litellm;
pub mod ollama;

#[cfg(test)]
mod tests;

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("{0}")]
    ClaudeCli(#[from] claude_cli::ClaudeCliError),
    #[error("ollama backend failed: {0}")]
    Ollama(String),
    #[error("{0}")]
    Unsupported(String),
}

/// One chat message. Roles mirror the Python client: `system`, `user`,
/// `assistant`, `tool` (any unknown role is treated as `user` by the
/// claude-cli flattener, like Python). `tool_calls` is set on an assistant
/// message that requested tool executions (the ollama/litellm loop).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl ChatMessage {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.to_string(),
            content: content.into(),
            tool_calls: None,
        }
    }
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self::new("system", content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new("user", content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new("assistant", content)
    }

    pub fn tool(content: impl Into<String>) -> Self {
        Self::new("tool", content)
    }
}

/// A function call the model requested (`{"function": {"name", "arguments"}}`
/// on the wire, exactly like the Python client's dict shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub function: FunctionCall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// The outcome of a single model turn — the full field set of the Python
/// `CompletionResult`.
#[derive(Debug, Clone)]
pub struct CompletionResult {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub runs_own_tool_loop: bool,
    /// Optional observability metadata (populated by backends that expose it).
    pub provider: String,
    pub duration_ms: Option<f64>,
    pub cost_usd: Option<f64>,
    pub usage: Option<serde_json::Value>,
}

/// Everything one `complete()` call needs beyond the messages.
#[derive(Debug, Clone)]
pub struct CompleteOptions<'a> {
    pub model: &'a str,
    /// Function-call schemas for the ollama/litellm tool loop.
    pub tools: Option<&'a [serde_json::Value]>,
    /// Resolved native tool names to expose to claude-cli via MCP.
    pub tool_names: &'a [String],
    pub temperature: f64,
    /// Hint the model to emit strict JSON (used by the router).
    pub json_mode: bool,
    /// Project root, required for the claude-cli MCP bridge.
    pub project_root: Option<&'a Path>,
    /// The conversation scope, forwarded to the MCP server's memory tools
    /// (Python reads the same value from its `active_scope` contextvar).
    pub scope: &'a str,
    /// Per-call timeout.
    pub timeout: Duration,
}

impl Default for CompleteOptions<'_> {
    fn default() -> Self {
        Self {
            model: "",
            tools: None,
            tool_names: &[],
            temperature: 0.2,
            json_mode: false,
            project_root: None,
            scope: "",
            timeout: Duration::from_secs(120),
        }
    }
}

/// Split a model string into (provider, model_name) — port of
/// `nerv.llm.client.parse_model`.
pub fn parse_model(model: &str) -> (&str, &str) {
    if let Some(rest) = model.strip_prefix("claude-cli:") {
        return ("claude-cli", rest);
    }
    if let Some(rest) = model.strip_prefix("local:") {
        return ("local", rest);
    }
    if let Some(rest) = model.strip_prefix("litellm:") {
        return ("litellm", rest);
    }
    // Bare cloud names (e.g. "claude-opus-4-8") route through litellm for
    // backwards compatibility with the reserved API path.
    ("litellm", model)
}

/// Run one assistant turn against the configured backend.
pub async fn complete(
    messages: &[ChatMessage],
    opts: &CompleteOptions<'_>,
) -> Result<CompletionResult, LlmError> {
    let (provider, model_name) = parse_model(opts.model);
    match provider {
        "claude-cli" => {
            let request = claude_cli::ClaudeCliRequest {
                model_name,
                json_mode: opts.json_mode,
                tool_names: opts.tool_names,
                project_root: opts.project_root,
                scope: opts.scope,
            };
            claude_cli::complete(messages, &request, opts.timeout).await
        }
        "local" => {
            ollama::complete(
                messages,
                model_name,
                opts.temperature,
                opts.json_mode,
                opts.tools,
                opts.timeout,
            )
            .await
        }
        // Multi-vendor cloud backends, served natively (the python litellm
        // sidecar is retired).
        _ => {
            litellm::complete(
                messages,
                model_name,
                opts.tools,
                opts.temperature,
                opts.json_mode,
                opts.timeout,
            )
            .await
        }
    }
}

/// Cut a string to `max_chars` characters (used to bound error text).
pub(crate) fn truncate(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}
