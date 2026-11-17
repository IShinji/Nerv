//! The routing decision type — the wire format shared with the Python
//! orchestrator (`RouteResult` there is the pydantic twin of this struct).

use serde::{Deserialize, Serialize};

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
    pub fn general_with_reply(reply: &str) -> Self {
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
