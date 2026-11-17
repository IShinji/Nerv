//! File-backed store for suspended high-risk actions awaiting user approval.
//!
//! Port of `brain/nerv/security/pending.py` (+ `state_version.py`). The store
//! is shared across processes: the Nerv MCP server (spawned by the Claude Code
//! CLI) enqueues suspended actions here, and the brain lists, executes, or
//! cancels them when the user replies with `confirm <id>` / `cancel <id>`.
//! Access is serialized with a file lock, and the JSON layout (including the
//! stamped schema_version) is the interface to the python twin.

use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tracing::error;

pub const CURRENT_SCHEMA_VERSION: i64 = 1;

/// A suspended tool invocation awaiting user confirmation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingRecord {
    pub id: i64,
    pub tool_name: String,
    pub arguments: Value,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub sender: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub justification: String,
}

/// A suspended action that nobody confirmed within this window is dropped, so
/// a stale approval can never fire long after the user forgot about it.
pub const DEFAULT_PENDING_TTL_SECONDS: f64 = 3600.0;

pub const MAX_JUSTIFICATION_CHARS: usize = 300;

/// Normalise a model-supplied justification for display: collapse all
/// whitespace (so it cannot fake extra chat lines) and cap the length.
/// Python rejects non-string values; the typed `Option<&str>` is the rust
/// analogue of that (a non-string argument cannot reach this call).
pub fn clean_justification(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let text = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > MAX_JUSTIFICATION_CHARS {
        let mut trimmed: String = text.chars().take(MAX_JUSTIFICATION_CHARS - 3).collect();
        trimmed = trimmed.trim_end().to_string();
        trimmed.push_str("...");
        trimmed
    } else {
        text
    }
}

pub struct PendingActionStore {
    ttl_seconds: f64,
    dir: PathBuf,
    path: PathBuf,
    lock_path: PathBuf,
}

impl PendingActionStore {
    pub fn new(project_root: &Path) -> Self {
        Self::with_ttl(project_root, DEFAULT_PENDING_TTL_SECONDS)
    }

    pub fn with_ttl(project_root: &Path, ttl_seconds: f64) -> Self {
        let dir = project_root.join("personal");
        Self {
            ttl_seconds,
            path: dir.join("pending_actions.json"),
            lock_path: dir.join("pending_actions.lock"),
            dir,
        }
    }

    fn locked<T>(&self, body: impl FnOnce() -> Result<T, StoreError>) -> Result<T, StoreError> {
        std::fs::create_dir_all(&self.dir)?;
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.lock_path)?;
        unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX);
        }
        let result = body();
        unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN);
        }
        result
    }

    fn read_raw(&self) -> Result<Value, StoreError> {
        if !self.path.exists() {
            return Ok(json!({"next_id": 1, "actions": []}));
        }
        let text = std::fs::read_to_string(&self.path)?;
        let data: Value = serde_json::from_str(&text).map_err(|e| {
            error!("Failed to parse {}: {e}", self.path.display());
            std::io::Error::other(e)
        })?;
        if is_newer_schema(&data, Some(&self.path)) {
            return Err(StoreError::NewerSchema);
        }
        Ok(data)
    }

    fn write_raw(&self, data: &Value) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let mut stamped = data.clone();
        if let Some(obj) = stamped.as_object_mut() {
            obj.insert("schema_version".to_string(), json!(CURRENT_SCHEMA_VERSION));
        }
        let tmp = self.dir.join(format!(
            ".pending-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&tmp, serde_json::to_string_pretty(&stamped)?)?;
        std::fs::rename(&tmp, &self.path)
    }

    /// Drop expired actions. Records without a timestamp count as expired.
    fn live(&self, actions: Vec<Value>) -> Vec<Value> {
        if self.ttl_seconds <= 0.0 {
            return actions;
        }
        let now = now_secs();
        actions
            .into_iter()
            .filter(|a| {
                now - a.get("created_at").and_then(|v| v.as_f64()).unwrap_or(0.0) < self.ttl_seconds
            })
            .collect()
    }

    /// Enqueue a suspended action and return its assigned record.
    #[allow(clippy::too_many_arguments)]
    pub fn add(
        &self,
        tool_name: &str,
        arguments: Value,
        reason: &str,
        sender: &str,
        channel: &str,
        scope: &str,
        justification: &str,
    ) -> Result<PendingRecord, StoreError> {
        self.locked(|| {
            let mut data = self.read_raw()?;
            let actions = self.live(
                data.get("actions")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default(),
            );
            let record = PendingRecord {
                id: data.get("next_id").and_then(|v| v.as_i64()).unwrap_or(1),
                tool_name: tool_name.to_string(),
                arguments,
                reason: reason.to_string(),
                sender: sender.to_string(),
                channel: channel.to_string(),
                scope: scope.to_string(),
                created_at: now_secs(),
                justification: clean_justification(Some(justification)),
            };
            data["next_id"] = json!(record.id + 1);
            let record_value = serde_json::to_value(&record)
                .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
            let mut all_actions = actions;
            all_actions.push(record_value);
            data["actions"] = json!(all_actions);
            self.write_raw(&data)?;
            Ok(record)
        })
    }

    /// Return all non-expired pending actions.
    pub fn list(&self) -> Vec<PendingRecord> {
        let read = self.locked(|| self.read_raw().map(|data| (data, ())));
        match read {
            Ok((data, _)) => {
                let known = [
                    "id",
                    "tool_name",
                    "arguments",
                    "reason",
                    "sender",
                    "channel",
                    "scope",
                    "created_at",
                    "justification",
                ];
                self.live(
                    data.get("actions")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default(),
                )
                .into_iter()
                .filter_map(|item| {
                    let filtered: serde_json::Map<String, Value> = item
                        .as_object()?
                        .iter()
                        .filter(|(k, _)| known.contains(&k.as_str()))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    serde_json::from_value(Value::Object(filtered)).ok()
                })
                .collect()
            }
            Err(StoreError::NewerSchema) => Vec::new(),
            Err(_) => Vec::new(),
        }
    }

    /// Return one pending action by id, or `None`.
    pub fn get(&self, action_id: i64) -> Option<PendingRecord> {
        self.list().into_iter().find(|rec| rec.id == action_id)
    }

    /// Remove and return a pending action by id, or `None` if absent.
    pub fn remove(&self, action_id: i64) -> Option<PendingRecord> {
        self.locked_inner(|| -> Result<Option<PendingRecord>, StoreError> {
            #[allow(unused)]
            let mut data = self.read_raw()?;
            let actions = data
                .get("actions")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let live = self.live(actions.clone());
            let removed = live
                .iter()
                .find(|a| a.get("id").and_then(|v| v.as_i64()) == Some(action_id))
                .cloned();
            let Some(removed) = removed else {
                return Ok(None);
            };
            data["actions"] = json!(actions
                .into_iter()
                .filter(|a| a.get("id").and_then(|v| v.as_i64()) != Some(action_id))
                .collect::<Vec<_>>());
            self.write_raw(&data)?;
            let known = [
                "id",
                "tool_name",
                "arguments",
                "reason",
                "sender",
                "channel",
                "scope",
                "created_at",
                "justification",
            ];
            let filtered: serde_json::Map<String, Value> = removed
                .as_object()
                .ok_or_else(|| std::io::Error::other("pending record is not an object"))?
                .iter()
                .filter(|(k, _)| known.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let record = serde_json::from_value(Value::Object(filtered))
                .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
            Ok(Some(record))
        })
        .unwrap_or(None)
    }

    fn locked_inner<T>(
        &self,
        body: impl FnOnce() -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        std::fs::create_dir_all(&self.dir)?;
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.lock_path)?;
        unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX);
        }
        let result = body();
        unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN);
        }
        result
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("the state file was written by a newer version of Nerv")]
    NewerSchema,
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// `json.dumps(arguments)` — python's default separators put a space after
/// the colons and commas, which the user-facing pending summaries inherit.
/// serde_json's compact output would differ, so walk the value here.
pub(crate) fn python_json_dumps(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => python_escape_string(s),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(python_json_dumps).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", python_escape_string(k), python_json_dumps(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

fn python_escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Whether `data` was written by a newer version; logs an error if so.
/// Port of `state_version.is_newer_schema`.
fn is_newer_schema(data: &Value, path: Option<&Path>) -> bool {
    let version = data
        .get("schema_version")
        .and_then(|v| v.as_i64())
        .unwrap_or(1);
    if version <= CURRENT_SCHEMA_VERSION {
        return false;
    }
    error!(
        "{} has schema_version {version} but this build supports up to \
         {CURRENT_SCHEMA_VERSION}; ignoring it and leaving it untouched",
        path.map(|p| p.display().to_string())
            .unwrap_or_else(|| "state file".to_string()),
    );
    true
}
