//! Data models for persisted chat memory — the serde twins of
//! `brain/nerv/memory/models.py` (the MCP server's python memory tools keep
//! reading and writing the same files).

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A single chat message in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default = "default_timestamp")]
    pub timestamp: NaiveDateTime,
    #[serde(default)]
    pub metadata: Value,
}

fn default_timestamp() -> NaiveDateTime {
    chrono::Local::now().naive_local()
}

impl StoredChatMessage {
    pub fn new(role: &str, content: &str) -> Self {
        Self {
            role: role.to_string(),
            content: content.to_string(),
            timestamp: default_timestamp(),
            metadata: Value::Null,
        }
    }
}

/// Daily conversation storage model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DailyConversation {
    /// `YYYY-MM-DD`.
    pub date: String,
    #[serde(default)]
    pub messages: Vec<StoredChatMessage>,
    #[serde(default)]
    pub summary: String,
}
