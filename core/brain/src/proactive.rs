//! Proactive background monitor — the source of Nerv reaching out first.
//!
//! Port of `brain/nerv/proactive.py`. Runs on the gateway heartbeat. It is
//! deliberately *deterministic and LLM-free* so heartbeats stay free (the
//! whole token-efficiency premise): it parses local state (calendar,
//! unfinished workflow runs) and emits reminders, deduplicated via a small
//! state file so the same thing is never surfaced twice.
//!
//! This module also owns the process-wide background-alert queue that the
//! turn loop and the workflow executor feed; the heartbeat drains it and
//! broadcasts to every channel (the E0 targeted pump handles the
//! sender-specific deliveries).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use serde_json::json;

use crate::tasks::TaskRunStore;

pub const DEFAULT_REMINDER_MINUTES: i64 = 60;

/// Scan local state and surface fresh, de-duplicated reminders.
pub struct ProactiveMonitor {
    root: PathBuf,
    calendar_path: PathBuf,
    state_path: PathBuf,
    enabled: bool,
    reminder_minutes: i64,
}

/// The process-wide background-alert queue (was python's
/// `_pending_notifications` + the `queue_notifications` RPC).
static PENDING_NOTIFICATIONS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Queue background alerts for the next heartbeat drain.
pub fn queue_notifications(items: &[String]) {
    PENDING_NOTIFICATIONS
        .lock()
        .unwrap()
        .extend(items.iter().cloned());
}

/// Drain the queued alerts (the heartbeat broadcast).
pub fn drain_notifications() -> Vec<String> {
    std::mem::take(&mut *PENDING_NOTIFICATIONS.lock().unwrap())
}

impl ProactiveMonitor {
    pub fn new(project_root: &Path) -> Self {
        let cfg = nerv_shared::config::NervConfig::load(project_root)
            .ok()
            .map(|c| c.proactive)
            .unwrap_or_default();
        Self {
            calendar_path: project_root.join("personal").join("calendar.ics"),
            state_path: project_root.join("personal").join("proactive_state.json"),
            enabled: cfg.enabled,
            reminder_minutes: cfg.calendar_reminder_minutes,
            root: project_root.to_path_buf(),
        }
    }

    /// Return newly-triggered reminders and persist what was surfaced.
    pub fn check(&self, now: Option<DateTime<Local>>) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }
        let now = now.unwrap_or_else(Local::now);
        let mut notified = load_notified(&self.state_path);
        let mut reminders: Vec<String> = Vec::new();

        for (key, message) in self.calendar_due(now) {
            if notified.insert(key) {
                reminders.push(message);
            }
        }

        for (key, message) in self.unfinished_workflows() {
            if notified.insert(key) {
                reminders.push(message);
            }
        }

        if !reminders.is_empty() {
            save_notified(&self.state_path, &notified);
        }
        reminders
    }

    /// Find events starting within the reminder window.
    fn calendar_due(&self, now: DateTime<Local>) -> Vec<(String, String)> {
        if !self.calendar_path.exists() {
            return Vec::new();
        }
        let Ok(body) = std::fs::read_to_string(&self.calendar_path) else {
            return Vec::new();
        };

        let window_end = now + chrono::Duration::minutes(self.reminder_minutes);
        let mut due: Vec<(String, String)> = Vec::new();
        let (mut uid, mut summary, mut start) = (String::new(), String::new(), String::new());
        for line in body.lines() {
            if let Some(rest) = line.strip_prefix("UID:") {
                uid = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("SUMMARY:") {
                summary = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("DTSTART:") {
                start = rest.to_string();
            } else if line.starts_with("END:VEVENT") {
                if let Some(start_dt) = parse_ics_dt(&start) {
                    if !summary.is_empty() && now <= start_dt && start_dt <= window_end {
                        let key = if uid.is_empty() {
                            format!("cal:{summary}{start}")
                        } else {
                            format!("cal:{uid}")
                        };
                        let when = start_dt.format("%H:%M");
                        due.push((key, format!("⏰ Reminder: '{summary}' at {when}.")));
                    }
                }
                uid = String::new();
                summary = String::new();
                start = String::new();
            }
        }
        due
    }

    /// Surface workflow runs that were interrupted and can be resumed.
    fn unfinished_workflows(&self) -> Vec<(String, String)> {
        let store = TaskRunStore::new(&self.root);
        store
            .list_incomplete()
            .iter()
            .map(|run| {
                let step_idx = run
                    .first_unfinished()
                    .map(|s| s.idx.to_string())
                    .unwrap_or_else(|| "?".to_string());
                (
                    format!("wf:{}", run.id),
                    format!(
                        "🔄 Unfinished workflow '{}' (run {}) is paused at step {}.",
                        run.workflow_name, run.id, step_idx
                    ),
                )
            })
            .collect()
    }
}

/// Parse a naive ICS datetime (`YYYYMMDDTHHMMSS`) in the local timezone.
fn parse_ics_dt(value: &str) -> Option<DateTime<Local>> {
    let naive = NaiveDateTime::parse_from_str(value.trim(), "%Y%m%dT%H%M%S").ok()?;
    Local.from_local_datetime(&naive).single()
}

/// The dedup state file schema (shared with the python implementation that
/// wrote it before the port; same keys, same version guard).
const CURRENT_SCHEMA_VERSION: i64 = 1;

fn load_notified(state_path: &Path) -> HashSet<String> {
    let Ok(body) = std::fs::read_to_string(state_path) else {
        return HashSet::new();
    };
    let Ok(data) = serde_json::from_str::<serde_json::Value>(&body) else {
        return HashSet::new();
    };
    if data
        .get("schema_version")
        .and_then(|v| v.as_i64())
        .is_some_and(|v| v > CURRENT_SCHEMA_VERSION)
    {
        return HashSet::new();
    }
    data.get("notified")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn save_notified(state_path: &Path, notified: &HashSet<String>) {
    if let Some(parent) = state_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if state_path.exists() {
        if let Ok(existing) = std::fs::read_to_string(state_path) {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&existing) {
                if data
                    .get("schema_version")
                    .and_then(|v| v.as_i64())
                    .is_some_and(|v| v > CURRENT_SCHEMA_VERSION)
                {
                    return;
                }
            }
        }
    }
    // Cap stored keys so the file can't grow without bound.
    let trimmed: Vec<&String> = notified.iter().take(500).collect();
    let body = serde_json::to_string(&json!({
        "schema_version": CURRENT_SCHEMA_VERSION,
        "notified": trimmed,
    }))
    .unwrap_or_default();
    let _ = std::fs::write(state_path, body);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_event(root: &Path, uid: &str, summary: &str, start: DateTime<Local>) {
        let cal = root.join("personal").join("calendar.ics");
        let _ = std::fs::create_dir_all(cal.parent().unwrap());
        let block = format!(
            "BEGIN:VEVENT\nUID:{uid}\nDTSTART:{}\nSUMMARY:{summary}\nEND:VEVENT\n",
            start.format("%Y%m%dT%H%M%S")
        );
        let existing = std::fs::read_to_string(&cal).unwrap_or_default();
        let body = if existing.is_empty() {
            format!("BEGIN:VCALENDAR\n{block}END:VCALENDAR\n")
        } else {
            existing + &block
        };
        std::fs::write(cal, body).unwrap();
    }

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).single().unwrap()
    }

    #[test]
    fn reminds_about_event_in_window_once() {
        let tmp = tempfile::tempdir().unwrap();
        let now = at(2026, 6, 1, 9, 0);
        write_event(
            tmp.path(),
            "e1",
            "Standup",
            now + chrono::Duration::minutes(15),
        );

        let monitor = ProactiveMonitor::new(tmp.path());
        let first = monitor.check(Some(now));
        assert!(first.iter().any(|m| m.contains("Standup")), "{first:?}");

        // Dedup: the same event is not surfaced again on the next heartbeat.
        let second = monitor.check(Some(now));
        assert!(second.is_empty());
    }

    #[test]
    fn ignores_events_outside_window() {
        let tmp = tempfile::tempdir().unwrap();
        let now = at(2026, 6, 1, 9, 0);
        write_event(
            tmp.path(),
            "far",
            "LaterMeeting",
            now + chrono::Duration::hours(5),
        );
        write_event(
            tmp.path(),
            "past",
            "OldMeeting",
            now - chrono::Duration::hours(1),
        );

        let monitor = ProactiveMonitor::new(tmp.path());
        assert!(monitor.check(Some(now)).is_empty());
    }

    #[test]
    fn disabled_monitor_is_silent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("core")).unwrap();
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("config.default.yaml"),
            tmp.path().join("core/config.default.yaml"),
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("config.yaml"),
            "proactive:\n  enabled: false\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                tmp.path().join("config.yaml"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let now = at(2026, 6, 1, 9, 0);
        write_event(
            tmp.path(),
            "e1",
            "Standup",
            now + chrono::Duration::minutes(10),
        );

        let monitor = ProactiveMonitor::new(tmp.path());
        assert!(monitor.check(Some(now)).is_empty());
    }

    #[test]
    fn surfaces_unfinished_workflow() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TaskRunStore::new(tmp.path());
        let run = store.create(
            "nightly",
            "x",
            &["step one".to_string(), "step two".to_string()],
        );

        let monitor = ProactiveMonitor::new(tmp.path());
        let messages = monitor.check(None);
        assert!(
            messages
                .iter()
                .any(|m| m.contains(&run.id) && m.contains("Unfinished")),
            "{messages:?}"
        );
    }

    #[test]
    fn notification_queue_round_trips() {
        // Other tests in this binary legitimately enqueue workflow alerts;
        // drain whatever is pending first, then assert the round trip.
        drain_notifications();
        queue_notifications(&["a".to_string(), "b".to_string()]);
        let drained = drain_notifications();
        assert!(
            drained.ends_with(&["a".to_string(), "b".to_string()]),
            "{drained:?}"
        );
        assert!(drain_notifications().is_empty());
    }
}
