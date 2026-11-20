//! Agent usage tracking for the self-evolution trigger (PRD Phase 2:
//! Agent 自我进化系统).
//!
//! Every dispatch to an agent bumps a counter in
//! `personal/evolution/usage.json`. When an agent crosses the threshold
//! since its last update, the proactive monitor surfaces a deterministic
//! reminder (LLM-free, so heartbeats stay free); running `evolve_agent`
//! resets the counter.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;
use tracing::debug;

/// The default dispatch count that triggers an evolution reminder.
pub const DEFAULT_EVOLUTION_THRESHOLD: u32 = 25;

/// The usage counters, persisted under `personal/evolution/usage.json`.
pub struct UsageTracker {
    path: PathBuf,
}

impl UsageTracker {
    pub fn new(project_root: &Path) -> Self {
        Self {
            path: project_root
                .join("personal")
                .join("evolution")
                .join("usage.json"),
        }
    }

    fn read(&self) -> BTreeMap<String, u32> {
        let Ok(body) = std::fs::read_to_string(&self.path) else {
            return BTreeMap::new();
        };
        let Ok(data) = serde_json::from_str::<serde_json::Value>(&body) else {
            return BTreeMap::new();
        };
        // Same newer-schema guard as every other state file.
        if data
            .get("schema_version")
            .and_then(|v| v.as_i64())
            .is_some_and(|v| v > CURRENT_SCHEMA_VERSION)
        {
            return BTreeMap::new();
        }
        data.get("counts")
            .and_then(|v| v.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n as u32)))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn write(&self, counts: &BTreeMap<String, u32>) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let body = serde_json::to_string_pretty(&json!({
            "schema_version": CURRENT_SCHEMA_VERSION,
            "counts": counts,
        }))
        .unwrap_or_default();
        let _ = std::fs::write(&self.path, body);
    }

    /// Count one dispatch to `agent`.
    pub fn record(&self, agent: &str) {
        if agent.trim().is_empty() {
            return;
        }
        let mut counts = self.read();
        *counts.entry(agent.to_string()).or_insert(0) += 1;
        self.write(&counts);
        debug!("Agent usage recorded: {agent}");
    }

    /// Reset an agent's counter (after an evolution).
    pub fn reset(&self, agent: &str) {
        let mut counts = self.read();
        if counts.remove(agent).is_some() {
            self.write(&counts);
        }
    }

    /// Agents whose dispatch count crossed `threshold` since their last
    /// update, with their counts, sorted by count descending.
    pub fn over_threshold(&self, threshold: u32) -> Vec<(String, u32)> {
        if threshold == 0 {
            return Vec::new();
        }
        let mut over: Vec<(String, u32)> = self
            .read()
            .into_iter()
            .filter(|(_, count)| *count >= threshold)
            .collect();
        over.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        over
    }
}

const CURRENT_SCHEMA_VERSION: i64 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_crosses_the_threshold() {
        let tmp = tempfile::tempdir().unwrap();
        let tracker = UsageTracker::new(tmp.path());
        for _ in 0..3 {
            tracker.record("Coder");
        }
        tracker.record("Writer");
        assert!(tracker
            .over_threshold(3)
            .contains(&("Coder".to_string(), 3)));
        assert!(tracker.over_threshold(3).iter().all(|(a, _)| a != "Writer"));
        assert!(tracker.over_threshold(100).is_empty());
    }

    #[test]
    fn reset_clears_one_agent_only() {
        let tmp = tempfile::tempdir().unwrap();
        let tracker = UsageTracker::new(tmp.path());
        for _ in 0..3 {
            tracker.record("Coder");
        }
        tracker.record("Writer");
        tracker.reset("Coder");
        assert!(tracker.over_threshold(3).is_empty());
        // The untouched agent keeps its count.
        assert_eq!(tracker.over_threshold(1), vec![("Writer".to_string(), 1)]);
    }

    #[test]
    fn empty_and_blank_agent_names_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let tracker = UsageTracker::new(tmp.path());
        tracker.record("");
        tracker.record("  ");
        assert!(tracker.over_threshold(1).is_empty());
    }
}
