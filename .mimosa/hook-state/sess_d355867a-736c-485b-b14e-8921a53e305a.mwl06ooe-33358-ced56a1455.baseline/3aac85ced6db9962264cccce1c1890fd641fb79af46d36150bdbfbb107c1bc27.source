//! Sanitization of noisy classifier output into stable routing fields.
//! Port of the sanitize half of `brain/nerv/router/router.py`.

use std::sync::LazyLock;

use regex::Regex;

use super::model::RouteResult;

/// Matches the first brace-delimited object in a noisy LLM response, mirroring
/// the Python extraction regex `r"\{[^}]+\}"`.
pub(super) static JSON_OBJECT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{[^}]+\}").expect("static regex must compile"));
pub(super) static NON_ROLE_CHARS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-z0-9_]+").expect("static regex must compile"));
pub(super) static RUN_OF_UNDERSCORES_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"_+").expect("static regex must compile"));

pub(super) fn canonical_agent_by_intent(intent: &str) -> &'static str {
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

impl super::Router {
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
}

/// `" ".join(message.strip().lower().split())` — collapse all whitespace.
pub(super) fn normalize_message(message: &str) -> String {
    message
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
