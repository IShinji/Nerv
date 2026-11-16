//! Unified LLM client — one model call, dispatched by model-string prefix.
//!
//! Seed port of `brain/nerv/llm/` covering what the Router needs: one plain
//! text/json completion turn. Tool loops, MCP bridging, usage/cost metadata
//! and the litellm cloud backend stay in Python until that module migrates.
//!
//! Model string scheme (the single switch point for swapping providers):
//!   "claude-cli:<alias>"   -> local Claude Code CLI (uses your subscription)
//!   "local:<ollama-model>" -> local Ollama HTTP (reserved; dormant)
//!   "litellm:<model>"      -> litellm cloud API (reserved, not ported yet)
//!   bare cloud name        -> treated as litellm

pub mod claude_cli;
pub mod ollama;

use std::time::Duration;

use serde::Serialize;
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
/// claude-cli flattener, like Python).
#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }
}

/// The outcome of a single model turn. Seed version: text content only —
/// tool calls and observability metadata arrive when the full llm module
/// migrates.
#[derive(Debug, Clone)]
pub struct CompletionResult {
    pub content: String,
}

/// Cut a string to `max_chars` characters (used to bound error text).
pub(crate) fn truncate(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
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
///
/// `timeout` bounds the whole call (per-call timeout, like Python).
pub async fn complete(
    messages: &[ChatMessage],
    model: &str,
    temperature: f64,
    json_mode: bool,
    timeout: Duration,
) -> Result<CompletionResult, LlmError> {
    let (provider, model_name) = parse_model(model);
    match provider {
        "claude-cli" => claude_cli::complete(messages, model_name, json_mode, timeout).await,
        "local" => ollama::complete(messages, model_name, temperature, json_mode, timeout).await,
        // Reserved backend, not ported yet. Callers treat this like any
        // backend failure (the Router falls back to keywords).
        _ => Err(LlmError::Unsupported(format!(
            "the litellm backend is not implemented in the Rust brain yet; \
             configure a claude-cli: or local: model instead (got {model:?})"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_splits_all_prefixes() {
        assert_eq!(parse_model("claude-cli:opus"), ("claude-cli", "opus"));
        assert_eq!(parse_model("local:qwen2.5:1.5b"), ("local", "qwen2.5:1.5b"));
        assert_eq!(
            parse_model("litellm:claude-opus-4-8"),
            ("litellm", "claude-opus-4-8")
        );
        assert_eq!(
            parse_model("claude-opus-4-8"),
            ("litellm", "claude-opus-4-8")
        );
    }
}
