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

use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use crate::llm::{self, ChatMessage};

pub const ROUTER_TIMEOUT: Duration = Duration::from_secs(60);
pub const DEFAULT_ROUTER_MODEL: &str = "claude-cli:opus";

const PROMPT_DIR: &str = "core/brain/prompts";
const SYSTEM_PROMPT_FILE: &str = "router_system.md";
const USER_TEMPLATE_FILE: &str = "router_user_template.txt";

/// Matches the first brace-delimited object in a noisy LLM response, mirroring
/// the Python extraction regex `r"\{[^}]+\}"`.
static JSON_OBJECT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{[^}]+\}").expect("static regex must compile"));
static NON_ROLE_CHARS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-z0-9_]+").expect("static regex must compile"));
static RUN_OF_UNDERSCORES_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"_+").expect("static regex must compile"));

fn canonical_agent_by_intent(intent: &str) -> &'static str {
    match intent {
        "code_generation" => "coder",
        "research" => "researcher",
        "writing" => "writer",
        "sysadmin" => "sysadmin",
        _ => "general",
    }
}

/// Port of `AGENT_ALIASES` — fuzzy agent names into stable built-in roles.
fn agent_alias(normalized: &str) -> Option<&'static str> {
    Some(match normalized {
        "general" | "assistant" | "personal_ai_assistant" | "chat" => "general",
        "code_generation" | "coder" | "developer" | "programmer" | "software_developer" => "coder",
        "research" | "researcher" | "search" | "analyst" => "researcher",
        "writing" | "writer" | "translator" | "summarizer" => "writer",
        "sysadmin" | "system_admin" | "system_administrator" | "devops" => "sysadmin",
        _ => return None,
    })
}

/// Port of `DIRECT_REPLY_EXACT_MESSAGES` — bilingual trivial social messages
/// that may keep a direct router reply.
fn is_direct_reply_exact(normalized: &str) -> bool {
    matches!(
        normalized,
        "hi" | "hello"
            | "hey"
            | "thanks"
            | "thank you"
            | "ok"
            | "okay"
            | "got it"
            | "bye"
            | "你好"
            | "您好"
            | "嗨"
            | "哈喽"
            | "谢谢"
            | "好的"
            | "收到"
            | "再见"
            | "对的"
            | "嗯"
            | "嗯嗯"
    )
}

/// Port of `QUESTION_MARKERS` — any hit suppresses direct replies.
const QUESTION_MARKERS: &[&str] = &[
    "?",
    "？",
    "what",
    "when",
    "where",
    "why",
    "how",
    "who",
    "which",
    "time",
    "date",
    "几点",
    "几号",
    "几月",
    "多少",
    "什么",
    "怎么",
    "谁",
    "哪",
    "何时",
    "为什么",
];

/// Result of intent classification by the Router.
///
/// Field names and defaults mirror the pydantic `RouteResult` in
/// `brain/nerv/models.py`; the wire format is snake_case JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteResult {
    pub intent: String,
    #[serde(default = "default_complexity")]
    pub complexity: String,
    #[serde(default)]
    pub model_tier: i64,
    #[serde(default = "default_agent_type")]
    pub agent_type: String,
    #[serde(default)]
    pub needs_capabilities: Vec<String>,
    #[serde(default = "default_execution_mode")]
    pub preferred_execution_mode: String,
    #[serde(default)]
    pub reply: String,
}

fn default_complexity() -> String {
    "low".to_string()
}

fn default_agent_type() -> String {
    "general".to_string()
}

fn default_execution_mode() -> String {
    "auto".to_string()
}

impl RouteResult {
    /// The sanitized "general" result used when nothing better is known.
    fn general_with_reply(reply: &str) -> Self {
        Self {
            intent: "general".to_string(),
            complexity: "low".to_string(),
            model_tier: 0,
            agent_type: "general".to_string(),
            needs_capabilities: Vec::new(),
            preferred_execution_mode: "auto".to_string(),
            reply: reply.to_string(),
        }
    }
}

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
        let result = llm::complete(&messages, &self.model, 0.1, true, ROUTER_TIMEOUT).await?;
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
        if let Some(m) = JSON_OBJECT_RE.find(content) {
            if let Ok(parsed) = serde_json::from_str::<RouteResult>(m.as_str()) {
                return parsed;
            }
        }

        warn!("Could not extract JSON from response, using fallback");
        RouteResult::general_with_reply(
            "I understood your message but had trouble classifying it. Could you rephrase?",
        )
    }

    /// Normalize noisy classifier output into stable routing fields.
    pub fn sanitize_route_result(&self, message: &str, mut result: RouteResult) -> RouteResult {
        result.agent_type = self.normalize_agent_type(&result.intent, &result.agent_type);
        if !self.should_use_direct_reply(message, &result.reply) {
            result.reply = String::new();
        }
        result
    }

    /// Map fuzzy agent names into stable built-in roles when possible.
    pub fn normalize_agent_type(&self, intent: &str, agent_type: &str) -> String {
        let lowered = agent_type.trim().to_lowercase();
        let normalized = NON_ROLE_CHARS_RE.replace_all(&lowered, "_");
        let normalized = RUN_OF_UNDERSCORES_RE.replace_all(&normalized, "_");
        let normalized = normalized.trim_matches('_');

        if intent == "general" {
            return "general".to_string();
        }

        if let Some(alias) = agent_alias(normalized) {
            return alias.to_string();
        }

        if normalized.is_empty() {
            return canonical_agent_by_intent(intent).to_string();
        }

        normalized.to_string()
    }

    /// Only allow direct router replies for trivial social messages.
    pub fn should_use_direct_reply(&self, message: &str, reply: &str) -> bool {
        if reply.trim().is_empty() {
            return false;
        }

        let normalized = normalize_message(message);
        if normalized.is_empty() {
            return true;
        }

        if QUESTION_MARKERS
            .iter()
            .any(|marker| normalized.contains(marker))
        {
            return false;
        }

        is_direct_reply_exact(&normalized)
    }

    /// Simple keyword-based fallback when the model backend is unavailable.
    pub fn fallback_classify(&self, message: &str) -> RouteResult {
        let message_lower = message.to_lowercase();

        const CODE_KEYWORDS: &[&str] = &[
            "code",
            "script",
            "function",
            "debug",
            "error",
            "bug",
            "program",
            "python",
            "rust",
            "javascript",
        ];
        const RESEARCH_KEYWORDS: &[&str] = &[
            "search", "find", "look up", "what is", "how does", "explain", "research",
        ];
        const WRITING_KEYWORDS: &[&str] = &[
            "write",
            "essay",
            "article",
            "translate",
            "summarize",
            "email",
        ];
        const SYSADMIN_KEYWORDS: &[&str] = &[
            "server", "deploy", "docker", "ssh", "nginx", "process", "disk", "memory",
        ];

        if CODE_KEYWORDS.iter().any(|kw| message_lower.contains(kw)) {
            return RouteResult {
                intent: "code_generation".to_string(),
                complexity: "medium".to_string(),
                model_tier: 1,
                agent_type: "coder".to_string(),
                needs_capabilities: Vec::new(),
                preferred_execution_mode: "auto".to_string(),
                reply: String::new(),
            };
        }
        if RESEARCH_KEYWORDS
            .iter()
            .any(|kw| message_lower.contains(kw))
        {
            return RouteResult {
                intent: "research".to_string(),
                complexity: "medium".to_string(),
                model_tier: 1,
                agent_type: "researcher".to_string(),
                needs_capabilities: Vec::new(),
                preferred_execution_mode: "auto".to_string(),
                reply: String::new(),
            };
        }
        if WRITING_KEYWORDS.iter().any(|kw| message_lower.contains(kw)) {
            return RouteResult {
                intent: "writing".to_string(),
                complexity: "medium".to_string(),
                model_tier: 1,
                agent_type: "writer".to_string(),
                needs_capabilities: Vec::new(),
                preferred_execution_mode: "auto".to_string(),
                reply: String::new(),
            };
        }
        if SYSADMIN_KEYWORDS
            .iter()
            .any(|kw| message_lower.contains(kw))
        {
            return RouteResult {
                intent: "sysadmin".to_string(),
                complexity: "medium".to_string(),
                model_tier: 0,
                agent_type: "sysadmin".to_string(),
                needs_capabilities: Vec::new(),
                preferred_execution_mode: "auto".to_string(),
                reply: String::new(),
            };
        }
        RouteResult::general_with_reply("")
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

/// `" ".join(message.strip().lower().split())` — collapse all whitespace.
fn normalize_message(message: &str) -> String {
    message
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> Router {
        Router::with_parts(
            "claude-cli:opus",
            include_str!("../prompts/router_system.md"),
            include_str!("../prompts/router_user_template.txt"),
        )
    }

    fn route_result(
        intent: &str,
        complexity: &str,
        model_tier: i64,
        agent_type: &str,
        reply: &str,
    ) -> RouteResult {
        RouteResult {
            intent: intent.to_string(),
            complexity: complexity.to_string(),
            model_tier,
            agent_type: agent_type.to_string(),
            needs_capabilities: Vec::new(),
            preferred_execution_mode: "auto".to_string(),
            reply: reply.to_string(),
        }
    }

    // --- TestRouterFallback ---

    #[test]
    fn fallback_code_intent_detected() {
        let result = router().fallback_classify("help me write a Python script");
        assert_eq!(result.intent, "code_generation");
        assert_eq!(result.agent_type, "coder");
    }

    #[test]
    fn fallback_research_intent_detected() {
        let result = router().fallback_classify("what is quantum computing");
        assert_eq!(result.intent, "research");
        assert_eq!(result.agent_type, "researcher");
    }

    #[test]
    fn fallback_writing_intent_detected() {
        let result = router().fallback_classify("write me an essay about AI");
        assert_eq!(result.intent, "writing");
        assert_eq!(result.agent_type, "writer");
    }

    #[test]
    fn fallback_sysadmin_intent_detected() {
        let result = router().fallback_classify("deploy this to the server");
        assert_eq!(result.intent, "sysadmin");
        assert_eq!(result.agent_type, "sysadmin");
    }

    #[test]
    fn fallback_general_intent() {
        let result = router().fallback_classify("hello there!");
        assert_eq!(result.intent, "general");
        assert_eq!(result.agent_type, "general");
    }

    #[test]
    fn fallback_empty_message_classified() {
        let result = router().fallback_classify("");
        assert_eq!(result.intent, "general");
    }

    // --- TestRouterParsing ---

    #[test]
    fn valid_json_parsed() {
        let content = r#"{"intent": "code_generation", "complexity": "medium", "model_tier": 1, "agent_type": "coder", "reply": ""}"#;
        let result = router().parse_response(content);
        assert_eq!(result.intent, "code_generation");
        assert_eq!(result.model_tier, 1);
    }

    #[test]
    fn json_with_extra_text_extracted() {
        let content = r#"Here is the result: {"intent": "general", "complexity": "low", "model_tier": 0, "agent_type": "general", "reply": "hello"}"#;
        let result = router().parse_response(content);
        assert_eq!(result.intent, "general");
    }

    #[test]
    fn invalid_json_returns_fallback() {
        let content = "This is not JSON at all";
        let result = router().parse_response(content);
        assert_eq!(result.intent, "general");
    }

    #[test]
    fn general_intent_agent_type_is_normalized() {
        let result = route_result("general", "low", 0, "personal AI assistant", "Hello!");
        let sanitized = router().sanitize_route_result("hello", result);
        assert_eq!(sanitized.agent_type, "general");
    }

    #[test]
    fn question_reply_is_suppressed() {
        let result = route_result("general", "low", 0, "search", "现在几点？");
        let sanitized = router().sanitize_route_result("现在几点", result);
        assert_eq!(sanitized.reply, "");
        assert_eq!(sanitized.agent_type, "general");
    }

    #[test]
    fn trivial_greeting_can_keep_direct_reply() {
        let result = route_result("general", "low", 0, "general", "你好，有什么可以帮你？");
        let sanitized = router().sanitize_route_result("你好", result);
        assert_eq!(sanitized.reply, "你好，有什么可以帮你？");
    }

    // --- TestRouteResult ---

    #[test]
    fn route_result_default_values() {
        let result: RouteResult = serde_json::from_str(r#"{"intent": "general"}"#).unwrap();
        assert_eq!(result.complexity, "low");
        assert_eq!(result.model_tier, 0);
        assert_eq!(result.agent_type, "general");
        assert!(result.needs_capabilities.is_empty());
        assert_eq!(result.preferred_execution_mode, "auto");
        assert_eq!(result.reply, "");
    }

    #[test]
    fn route_result_serialization() {
        let result = RouteResult {
            intent: "code_generation".to_string(),
            complexity: "high".to_string(),
            model_tier: 2,
            agent_type: "coder".to_string(),
            needs_capabilities: vec!["filesystem.read".to_string(), "local.exec".to_string()],
            preferred_execution_mode: "auto".to_string(),
            reply: "I can help with that".to_string(),
        };
        let data = serde_json::to_value(&result).unwrap();
        assert_eq!(data["intent"], "code_generation");
        assert_eq!(data["model_tier"], 2);
        assert_eq!(
            data["needs_capabilities"],
            serde_json::json!(["filesystem.read", "local.exec"])
        );
        assert_eq!(data["preferred_execution_mode"], "auto");
    }

    // --- Golden eval (port of eval.py offline mode / test_eval_routing.py) ---

    struct RoutingCase {
        message: &'static str,
        raw_model_json: &'static str,
        expect_agent: &'static str,
        expect_reply_suppressed: bool,
    }

    /// Port of `GOLDEN_ROUTING`.
    fn golden_routing() -> Vec<RoutingCase> {
        vec![
            RoutingCase {
                message: "help me debug this python function",
                raw_model_json: r#"{"intent":"code_generation","complexity":"medium","model_tier":1,"agent_type":"developer","reply":""}"#,
                expect_agent: "coder",
                expect_reply_suppressed: false,
            },
            RoutingCase {
                message: "compare postgres and mysql for analytics",
                raw_model_json: r#"{"intent":"research","complexity":"high","model_tier":2,"agent_type":"analyst","reply":""}"#,
                expect_agent: "researcher",
                expect_reply_suppressed: false,
            },
            RoutingCase {
                message: "translate this paragraph to french",
                raw_model_json: r#"{"intent":"writing","complexity":"low","model_tier":1,"agent_type":"translator","reply":""}"#,
                expect_agent: "writer",
                expect_reply_suppressed: false,
            },
            RoutingCase {
                message: "restart nginx on the server",
                raw_model_json: r#"{"intent":"sysadmin","complexity":"medium","model_tier":1,"agent_type":"devops","reply":""}"#,
                expect_agent: "sysadmin",
                expect_reply_suppressed: false,
            },
            RoutingCase {
                message: "你好",
                raw_model_json: r#"{"intent":"general","complexity":"low","model_tier":0,"agent_type":"personal AI assistant","reply":"你好，有什么可以帮你？"}"#,
                expect_agent: "general",
                expect_reply_suppressed: false,
            },
            RoutingCase {
                message: "现在几点",
                raw_model_json: r#"{"intent":"general","complexity":"low","model_tier":0,"agent_type":"assistant","reply":"现在几点？"}"#,
                expect_agent: "general",
                expect_reply_suppressed: true,
            },
        ]
    }

    #[test]
    fn golden_routing_pipeline_is_fully_correct() {
        let r = router();
        let cases = golden_routing();
        assert!(cases.len() >= 5);
        for case in &cases {
            let parsed = r.parse_response(case.raw_model_json);
            let result = r.sanitize_route_result(case.message, parsed);

            let mut problems = Vec::new();
            if result.agent_type != case.expect_agent {
                problems.push(format!(
                    "agent {:?}!={:?}",
                    result.agent_type, case.expect_agent
                ));
            }
            if case.expect_reply_suppressed && !result.reply.is_empty() {
                problems.push("reply not suppressed".to_string());
            }
            assert!(
                problems.is_empty(),
                "{:?}: {}",
                case.message,
                problems.join(", ")
            );
        }
    }

    #[test]
    fn golden_eval_reports_failures_for_wrong_expectations() {
        // Port of test_eval_report_tracks_failures: a deliberately wrong
        // expectation must be caught, not silently passed.
        let r = router();
        let parsed =
            r.parse_response(r#"{"intent":"code_generation","agent_type":"developer","reply":""}"#);
        let result = r.sanitize_route_result("x", parsed);
        assert_ne!(result.agent_type, "researcher");
    }

    // --- classify entry points ---

    #[tokio::test]
    async fn empty_message_gets_the_direct_help_reply() {
        let result = router().classify("").await;
        assert_eq!(result.intent, "general");
        assert_eq!(result.agent_type, "general");
        assert_eq!(
            result.reply,
            "I received an empty message. How can I help you?"
        );
    }

    #[tokio::test]
    async fn whitespace_only_message_gets_the_direct_help_reply() {
        let result = router().classify("   \n\t").await;
        assert_eq!(
            result.reply,
            "I received an empty message. How can I help you?"
        );
    }

    #[tokio::test]
    async fn unsupported_backend_falls_back_to_keywords() {
        // The litellm backend is not implemented in the Rust brain yet; the
        // Router must treat that like any backend failure.
        let r = Router::with_parts(
            "litellm:claude-opus-4-8",
            include_str!("../prompts/router_system.md"),
            include_str!("../prompts/router_user_template.txt"),
        );
        let result = r.classify("help me debug this python function").await;
        assert_eq!(result.intent, "code_generation");
        assert_eq!(result.agent_type, "coder");
    }

    #[test]
    fn user_prompt_renders_like_python_str_format() {
        let template = include_str!("../prompts/router_user_template.txt");
        let rendered = render_user_prompt(template, "你好");
        assert!(rendered.starts_with("Classify this message and return JSON:"));
        assert!(rendered.contains("\n\nMessage: 你好\n\n"));
        assert!(rendered.ends_with(r#"Return format: {"intent": "...", "complexity": "...", "model_tier": N, "agent_type": "...", "needs_capabilities": ["..."], "preferred_execution_mode": "...", "reply": "..."}"#));
        assert!(!rendered.contains("{{"));
    }

    /// Live spot-check against the configured backend: `cargo test -p
    /// nerv-brain -- --ignored`. Port of `python -m nerv.eval`.
    #[tokio::test]
    #[ignore = "requires the configured router backend"]
    async fn live_routing_eval() {
        let project_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let r = Router::new(&project_root).expect("router loads from repo layout");
        for case in golden_routing() {
            let result = r.classify(case.message).await;
            assert_eq!(
                result.agent_type, case.expect_agent,
                "message {:?}",
                case.message
            );
        }
    }
}
