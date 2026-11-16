//! Router — intent classification via the configured model backend.
//!
//! Port of `brain/nerv/router/router.py` (behavior-preserving). Classifies
//! user messages into intents, complexity levels, and suggested model tiers.
//! The backend model is a pure config switch (claude-cli / local Ollama /
//! litellm) resolved through the unified LLM client; a keyword fallback keeps
//! routing working fully offline.
//!
//! Prompts are data files under `<project_root>/core/brain/prompts/` so they
//! can be iterated on without a compile (CLAUDE.md "Rust migration rules").

mod fallback;
mod model;
mod sanitize;

#[cfg(test)]
mod tests;

pub use model::RouteResult;

use std::path::Path;
use std::time::Duration;

use tracing::{debug, error, info, warn};

use crate::llm::{self, ChatMessage};

pub const ROUTER_TIMEOUT: Duration = Duration::from_secs(60);
pub const DEFAULT_ROUTER_MODEL: &str = "claude-cli:opus";

const PROMPT_DIR: &str = "core/brain/prompts";
const SYSTEM_PROMPT_FILE: &str = "router_system.md";
const USER_TEMPLATE_FILE: &str = "router_user_template.txt";

#[derive(Debug, thiserror::Error)]
pub enum RouterInitError {
    #[error("failed to load config for the router model: {0}")]
    Config(#[from] nerv_shared::config::ConfigError),
    #[error("failed to read router prompt data file: {0}")]
    Prompt(#[from] std::io::Error),
}

/// Classify user messages using the configured model backend.
pub struct Router {
    /// Provider-prefixed model string, frozen at construction (like Python).
    pub model: String,
    system_prompt: String,
    user_template: String,
}

impl Router {
    /// Build a router that resolves the model from config (env override wins)
    /// and loads the prompt data files from `<project_root>/core/brain/prompts/`.
    pub fn new(project_root: &Path) -> Result<Self, RouterInitError> {
        let model = resolve_router_model(project_root)?;
        let prompt_dir = project_root.join(PROMPT_DIR);
        let system_prompt = std::fs::read_to_string(prompt_dir.join(SYSTEM_PROMPT_FILE))?;
        let user_template = std::fs::read_to_string(prompt_dir.join(USER_TEMPLATE_FILE))?;
        Ok(Self::with_parts(model, system_prompt, user_template))
    }

    /// Build a router from explicit parts — used by tests and by hosts that
    /// already resolved the model and prompts themselves.
    pub fn with_parts(
        model: impl Into<String>,
        system_prompt: impl Into<String>,
        user_template: impl Into<String>,
    ) -> Self {
        let model = model.into();
        info!("Router initialized: model={model}");
        Self {
            model,
            system_prompt: system_prompt.into(),
            user_template: user_template.into(),
        }
    }

    /// Classify a user message into an intent + routing decision. Any backend
    /// failure falls back to the keyword classifier, so routing never fails.
    pub async fn classify(&self, message: &str) -> RouteResult {
        if message.trim().is_empty() {
            return RouteResult::general_with_reply(
                "I received an empty message. How can I help you?",
            );
        }

        let user_prompt = render_user_prompt(&self.user_template, message);
        match self.classify_via_model(&user_prompt).await {
            Ok(result) => self.sanitize_route_result(message, result),
            Err(e) => {
                error!("Router model call failed, using fallback: {e}");
                self.sanitize_route_result(message, self.fallback_classify(message))
            }
        }
    }

    async fn classify_via_model(&self, user_prompt: &str) -> Result<RouteResult, llm::LlmError> {
        let messages = [
            ChatMessage::system(&self.system_prompt),
            ChatMessage::user(user_prompt),
        ];
        let result = llm::complete(
            &messages,
            &llm::CompleteOptions {
                model: &self.model,
                temperature: 0.1,
                json_mode: true,
                timeout: ROUTER_TIMEOUT,
                ..Default::default()
            },
        )
        .await?;
        debug!("Router raw response: {}", result.content);
        Ok(self.parse_response(&result.content))
    }

    /// Parse the JSON response from the LLM.
    pub fn parse_response(&self, content: &str) -> RouteResult {
        match serde_json::from_str::<RouteResult>(content) {
            Ok(result) => result,
            Err(e) => {
                warn!("Failed to parse LLM response as JSON: {content} — {e}");
                // Try to extract JSON from the response if wrapped in text
                self.extract_json(content)
            }
        }
    }

    /// Try to extract JSON from a response that may contain extra text.
    pub fn extract_json(&self, content: &str) -> RouteResult {
        if let Some(m) = sanitize::JSON_OBJECT_RE.find(content) {
            if let Ok(parsed) = serde_json::from_str::<RouteResult>(m.as_str()) {
                return parsed;
            }
        }

        warn!("Could not extract JSON from response, using fallback");
        RouteResult::general_with_reply(
            "I understood your message but had trouble classifying it. Could you rephrase?",
        )
    }
}

/// Resolve the router model string: env override wins, then the config
/// `models.router.model` string, then the default. Port of
/// `nerv.config.get_router_model`.
fn resolve_router_model(project_root: &Path) -> Result<String, RouterInitError> {
    if let Ok(model) = std::env::var("NERV_ROUTER_MODEL") {
        if !model.is_empty() {
            return Ok(model);
        }
    }
    let config = nerv_shared::config::NervConfig::load(project_root)?;
    let model = config.models.router.model.trim().to_string();
    if !model.is_empty() {
        return Ok(model);
    }
    Ok(DEFAULT_ROUTER_MODEL.to_string())
}

/// Render the user prompt from the template data file.
///
/// The template keeps Python `str.format` brace escapes, so `{{`/`}}` are
/// collapsed first and then the single `{message}` slot is substituted — the
/// substituted text is never rescanned, matching Python's single-pass format.
fn render_user_prompt(template: &str, message: &str) -> String {
    template
        .replace("{{", "{")
        .replace("}}", "}")
        .replace("{message}", message)
}
