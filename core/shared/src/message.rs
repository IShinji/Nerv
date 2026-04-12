use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Unified message format — all channel adapters convert to/from this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub channel: ChannelType,
    pub sender: String,
    pub content: MessageContent,
    pub timestamp: DateTime<Utc>,
    pub reply_to: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Message {
    pub fn new_text(
        channel: ChannelType,
        sender: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            channel,
            sender: sender.into(),
            content: MessageContent::Text(text.into()),
            timestamp: Utc::now(),
            reply_to: None,
            metadata: HashMap::new(),
        }
    }

    /// Extract the text content, if this is a text message.
    pub fn text(&self) -> Option<&str> {
        match &self.content {
            MessageContent::Text(t) => Some(t.as_str()),
            _ => None,
        }
    }
}

/// Supported channel types — extensible via new variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelType {
    Telegram,
    Email,
    Slack,
    WebChat,
    Cli,
}

impl std::fmt::Display for ChannelType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Telegram => write!(f, "telegram"),
            Self::Email => write!(f, "email"),
            Self::Slack => write!(f, "slack"),
            Self::WebChat => write!(f, "webchat"),
            Self::Cli => write!(f, "cli"),
        }
    }
}

/// Message content variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum MessageContent {
    Text(String),
    Image {
        url: String,
        caption: Option<String>,
    },
    File {
        url: String,
        filename: String,
    },
    Voice {
        url: String,
        duration_secs: Option<f64>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_new_text_generates_unique_ids() {
        let m1 = Message::new_text(ChannelType::Cli, "user", "hello");
        let m2 = Message::new_text(ChannelType::Cli, "user", "world");
        assert_ne!(m1.id, m2.id);
    }

    #[test]
    fn test_message_text_extraction() {
        let msg = Message::new_text(ChannelType::Telegram, "user123", "test message");
        assert_eq!(msg.text(), Some("test message"));
        assert_eq!(msg.sender, "user123");
        assert_eq!(msg.channel, ChannelType::Telegram);
    }

    #[test]
    fn test_message_serialization_roundtrip() {
        let msg = Message::new_text(ChannelType::Cli, "user", "hello");
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, msg.id);
        assert_eq!(deserialized.text(), Some("hello"));
    }

    #[test]
    fn test_channel_type_display() {
        assert_eq!(ChannelType::Telegram.to_string(), "telegram");
        assert_eq!(ChannelType::Cli.to_string(), "cli");
    }
}
