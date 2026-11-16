//! One-line-per-request call log (`personal/logs/trace.jsonl`).
//!
//! Port of `brain/nerv/request_trace.py` plus the event-emission half of
//! `brain/nerv/observability.py` (`log_event`/`new_trace_id`) that the turn
//! loop uses. Privacy contract: a record holds only names, sizes, counts and
//! categories — never message text, model replies, tool arguments or results,
//! or secrets. Writing can never fail or delay a request.

use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::json;
use tracing::warn;

pub const TRACE_RELATIVE_PATH: &str = "personal/logs/trace.jsonl";
pub const TRACE_MAX_BYTES: u64 = 5 * 1024 * 1024;
pub const TRACE_KEEP_ROTATED: u32 = 3;

/// What is known about one request; serialized as one JSON line.
#[derive(Debug)]
pub struct TraceRecord {
    pub channel: String,
    pub scope: String,
    pub tier: Option<i64>,
    pub model: String,
    pub provider: String,
    pub est_input_tokens: i64,
    pub tools_called: Vec<String>,
    pub cost_usd: Option<f64>,
    pub pending_created: bool,
    pub delegated: bool,
    pub error: String,
    started: std::time::Instant,
}

impl Default for TraceRecord {
    fn default() -> Self {
        Self {
            channel: String::new(),
            scope: String::new(),
            tier: None,
            model: String::new(),
            provider: String::new(),
            est_input_tokens: 0,
            tools_called: Vec::new(),
            cost_usd: None,
            pending_created: false,
            delegated: false,
            error: String::new(),
            started: std::time::Instant::now(),
        }
    }
}

impl TraceRecord {
    pub fn new(channel: &str, scope: &str) -> Self {
        Self {
            channel: channel.to_string(),
            scope: scope.to_string(),
            started: std::time::Instant::now(),
            ..Default::default()
        }
    }

    /// Build the JSON record (fixed field set, no free text).
    fn to_record(&self) -> serde_json::Value {
        json!({
            "ts": Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
            "channel": self.channel,
            "scope": self.scope,
            "tier": self.tier,
            "model": self.model,
            "provider": self.provider,
            "est_input_tokens": self.est_input_tokens,
            "tools_called": self.tools_called,
            "cost_usd": self.cost_usd,
            "pending_created": self.pending_created,
            "delegated": self.delegated,
            "duration_ms": round1(self.started.elapsed().as_secs_f64() * 1000.0),
            "error": if self.error.is_empty() {
                serde_json::Value::Null
            } else {
                json!(self.error)
            },
        })
    }

    /// Append the request's line; log a warning and return on any failure.
    /// Port of `write_trace` — file lock, size-capped rotation, mode 0600.
    pub fn write(&self, project_root: &Path) {
        let outcome = (|| -> std::io::Result<()> {
            let path = project_root.join(TRACE_RELATIVE_PATH);
            let dir = path
                .parent()
                .ok_or_else(|| std::io::Error::other("trace path has no parent"))?;
            std::fs::create_dir_all(dir)?;
            set_mode_700(dir);
            let line = format!("{}\n", self.to_record());
            #[cfg(unix)]
            {
                let lock_path = dir.join("trace.lock");
                let lock_file = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open(&lock_path)?;
                set_mode_600(&lock_path);
                unsafe {
                    libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX);
                }
                let result = append_line(&path, &line);
                unsafe {
                    libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN);
                }
                result?;
            }
            #[cfg(not(unix))]
            {
                append_line(&path, &line)?;
            }
            Ok(())
        })();
        if let Err(exc) = outcome {
            // tracing must never affect a request
            warn!("Could not write request trace: {exc}");
        }
    }
}

fn append_line(path: &PathBuf, line: &str) -> std::io::Result<()> {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() >= TRACE_MAX_BYTES {
            rotate(path);
        }
    }
    set_mode_600(path);
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())
}

/// Shift `trace.jsonl` -> `.1` -> `.2` -> `.3`, dropping the oldest.
fn rotate(path: &Path) {
    for index in (1..=TRACE_KEEP_ROTATED).rev() {
        let source = if index == 1 {
            path.to_path_buf()
        } else {
            sibling(path, index - 1)
        };
        let target = sibling(path, index);
        if source.exists() {
            let _ = std::fs::rename(&source, &target);
        }
    }
}

fn sibling(path: &Path, index: u32) -> PathBuf {
    PathBuf::from(format!("{}.{index}", path.display()))
}

fn set_mode_600(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn set_mode_700(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Rough input size: characters / 4. Only lengths are used, never text.
/// (The CJK-aware estimator lives with the context assembly in python.)
pub fn estimate_tokens(messages: &[crate::llm::ChatMessage]) -> i64 {
    let total: usize = messages.iter().map(|m| m.content.chars().count()).sum();
    (total / 4) as i64
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// Return a short correlation id for one user turn.
pub fn new_trace_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

/// Emit a single structured event as a JSON line (stderr via tracing, plus
/// `NERV_TRACE_FILE` when set). Port of `log_event`.
pub fn log_event(event: &str, mut fields: serde_json::Value) {
    fields["event"] = json!(event);
    fields["ts"] = json!(round1(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0),
    ));
    let record = fields;
    let line = serde_json::to_string(&record).unwrap_or_default();
    tracing::info!("{line}");

    if let Ok(trace_file) = std::env::var("NERV_TRACE_FILE") {
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&trace_file)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "{line}")
            });
    }
}
