//! File-backed store for user-created scheduled messages (PRD v2 E2).
//!
//! Each message is one JSON file under
//! `personal/scheduled_messages/<id>.json`. The brain creates and cancels
//! messages by writing state transitions; the gateway polls for due entries
//! and sends them as regular user messages. "Most recent due only": if the
//! gateway was asleep for days, exactly one message fires — no history
//! enumeration.
//!
//! Schedule format: a `next_fire` ISO-8601 local datetime. After firing, the
//! gateway recomputes `next_fire` from the `recurrence` field (`""` = one
//! shot; `daily` / `weekly` / `monthly` recur) using the same timezone, so
//! DST transitions shift naturally.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const CURRENT_SCHEMA_VERSION: i64 = 1;
/// Status values.
pub const ACTIVE: &str = "active";
pub const CANCELLED: &str = "cancelled";

/// One user-created scheduled message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledMessage {
    pub id: String,
    /// The message text, sent as a user message at the due time.
    pub message: String,
    /// The channel to deliver the response on.
    pub channel: String,
    /// The sender identity to use (must be in the allow-list).
    pub sender: String,
    /// `""` = one shot; `daily` / `weekly` / `monthly` recur.
    #[serde(default)]
    pub recurrence: String,
    /// The next fire time, ISO-8601 local (e.g. `2026-06-01T09:00:00`).
    pub next_fire: String,
    pub status: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub schema_version: i64,
}

/// File-backed store for scheduled messages.
pub struct ScheduledMessageStore {
    dir: PathBuf,
}

impl ScheduledMessageStore {
    pub fn new(project_root: &Path) -> Self {
        Self {
            dir: project_root.join("personal").join("scheduled_messages"),
        }
    }

    fn path(&self, id: &str) -> Option<PathBuf> {
        // Reject ids that could traverse out of the store directory.
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return None;
        }
        Some(self.dir.join(format!("{id}.json")))
    }

    /// Save a scheduled message atomically with the schema stamp.
    pub fn save(&self, message: &ScheduledMessage) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let Some(path) = self.path(&message.id) else {
            return Err("invalid scheduled message id".to_string());
        };
        let mut body =
            serde_json::to_value(message).map_err(|e| format!("Failed to serialize: {e}"))?;
        if let Some(obj) = body.as_object_mut() {
            obj.insert(
                "schema_version".to_string(),
                serde_json::json!(CURRENT_SCHEMA_VERSION),
            );
        }
        let tmp = self.dir.join(format!(".{}-tmp", message.id));
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(&body).unwrap_or_default(),
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    /// Load one message by id; `None` when absent or written by a newer build.
    pub fn get(&self, id: &str) -> Option<ScheduledMessage> {
        let path = self.path(id)?;
        let text = std::fs::read_to_string(&path).ok()?;
        let data: serde_json::Value = serde_json::from_str(&text).ok()?;
        if data
            .get("schema_version")
            .and_then(|v| v.as_i64())
            .unwrap_or(1)
            > CURRENT_SCHEMA_VERSION
        {
            return None;
        }
        serde_json::from_value(data).ok()
    }

    /// All active messages.
    pub fn list_active(&self) -> Vec<ScheduledMessage> {
        let mut found = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if let Some(id) = path.file_stem().map(|s| s.to_string_lossy().to_string()) {
                    if let Some(message) = self.get(&id) {
                        if message.status == ACTIVE {
                            found.push(message);
                        }
                    }
                }
            }
        }
        found.sort_by(|a, b| a.next_fire.cmp(&b.next_fire));
        found
    }
}

/// Whether `next_fire` is due (past or equal to `now`, compared as ISO-8601
/// strings so the check is timezone-aware without parsing).
pub fn is_due(next_fire: &str, now: &str) -> bool {
    next_fire <= now
}

/// Compute the next fire time for a recurring message, one recurrence after
/// `fired_at`. Returns `None` for one-shot messages.
pub fn next_occurrence(fired_at: &str, recurrence: &str) -> Option<String> {
    let parsed = chrono::NaiveDateTime::parse_from_str(fired_at, "%Y-%m-%dT%H:%M:%S").ok()?;
    let next = match recurrence {
        "daily" => parsed + chrono::Duration::days(1),
        "weekly" => parsed + chrono::Duration::weeks(1),
        "monthly" => parsed + chrono::Duration::days(30),
        _ => return None,
    };
    Some(next.format("%Y-%m-%dT%H:%M:%S").to_string())
}
