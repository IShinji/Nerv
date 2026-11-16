//! Keyword fallback classifier — keeps routing alive when the model backend
//! fails. Port of `_fallback_classify` in `brain/nerv/router/router.py`.

use super::model::RouteResult;

impl super::Router {
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
