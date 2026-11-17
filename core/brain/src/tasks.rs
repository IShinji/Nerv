//! Persistent task-run state so multi-step workflows survive crashes and
//! restarts. Port of `brain/nerv/tasks/store.py` (+ `state_version.py`'s
//! newer-schema guard, already ported in `tools::pending_store`).
//!
//! Each workflow execution is a `TaskRun` persisted as JSON under
//! `personal/tasks/<run_id>.json`. Step results are written after every step,
//! so an interrupted run resumes from the first step that is not yet done.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::error;

pub const CURRENT_SCHEMA_VERSION: i64 = 1;

// Step / run status values.
pub const PENDING: &str = "pending";
pub const RUNNING: &str = "running";
pub const DONE: &str = "done";
pub const FAILED: &str = "failed";
/// Stopped on a step that was queued for the user's confirmation instead of run.
pub const SUSPENDED: &str = "suspended";

/// Execution state for a single workflow step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepState {
    pub idx: i64,
    pub description: String,
    #[serde(default = "default_pending")]
    pub status: String,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub attempts: i64,
}

fn default_pending() -> String {
    PENDING.to_string()
}

/// A resumable multi-step workflow execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRun {
    pub id: String,
    pub workflow_name: String,
    pub initial_input: String,
    #[serde(default = "default_running")]
    pub status: String,
    #[serde(default)]
    pub steps: Vec<StepState>,
    #[serde(default = "now_secs")]
    pub created_at: f64,
    #[serde(default = "now_secs")]
    pub updated_at: f64,
    #[serde(default)]
    pub schema_version: i64,
}

fn default_running() -> String {
    RUNNING.to_string()
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

impl TaskRun {
    /// The earliest step that still needs to run.
    pub fn first_unfinished(&self) -> Option<&StepState> {
        self.steps.iter().find(|s| s.status != DONE)
    }

    /// Whether every step is done.
    pub fn is_complete(&self) -> bool {
        self.steps.iter().all(|s| s.status == DONE)
    }
}

/// Filesystem-backed persistence for task runs.
pub struct TaskRunStore {
    dir: PathBuf,
}

impl TaskRunStore {
    pub fn new(project_root: &Path) -> Self {
        Self {
            dir: project_root.join("personal").join("tasks"),
        }
    }

    fn path(&self, run_id: &str) -> PathBuf {
        self.dir.join(format!("{run_id}.json"))
    }

    /// Create and persist a fresh run for a workflow's steps.
    pub fn create(
        &self,
        workflow_name: &str,
        initial_input: &str,
        step_descriptions: &[String],
    ) -> TaskRun {
        let run = TaskRun {
            id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
            workflow_name: workflow_name.to_string(),
            initial_input: initial_input.to_string(),
            status: RUNNING.to_string(),
            steps: step_descriptions
                .iter()
                .enumerate()
                .map(|(i, desc)| StepState {
                    idx: (i + 1) as i64,
                    description: desc.clone(),
                    status: PENDING.to_string(),
                    result: String::new(),
                    attempts: 0,
                })
                .collect(),
            created_at: now_secs(),
            updated_at: now_secs(),
            schema_version: CURRENT_SCHEMA_VERSION,
        };
        self.save(&run);
        run
    }

    /// Atomically persist the current run state.
    pub fn save(&self, run: &mut TaskRun) {
        run.updated_at = now_secs();
        run.schema_version = CURRENT_SCHEMA_VERSION;
        if let Err(e) = std::fs::create_dir_all(&self.dir) {
            error!("Failed to create the tasks directory: {e}");
            return;
        }
        let path = self.path(&run.id);
        if Self::on_disk_is_newer(&path) {
            return;
        }
        let tmp = path.with_extension("json.tmp");
        match serde_json::to_string_pretty(run) {
            Ok(body) => {
                if let Err(e) = std::fs::write(&tmp, body) {
                    error!("Failed to write task run {}: {e}", run.id);
                    return;
                }
                if let Err(e) = std::fs::rename(&tmp, &path) {
                    error!("Failed to persist task run {}: {e}", run.id);
                }
            }
            Err(e) => error!("Failed to serialize task run {}: {e}", run.id),
        }
    }

    /// Whether an existing file was written by a newer version (keep it).
    fn on_disk_is_newer(path: &Path) -> bool {
        if !path.exists() {
            return false;
        }
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(data) => data
                    .get("schema_version")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(1)
                    > CURRENT_SCHEMA_VERSION,
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    /// Load a run by id, or `None` if absent/corrupt/newer.
    pub fn get(&self, run_id: &str) -> Option<TaskRun> {
        let path = self.path(run_id);
        if !path.exists() {
            return None;
        }
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

    /// Return all runs that have not finished (for resume on startup).
    pub fn list_incomplete(&self) -> Vec<TaskRun> {
        if !self.dir.exists() {
            return Vec::new();
        }
        let mut runs = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Some(run) = self.get(
                        path.file_stem().unwrap_or_default().to_string_lossy().as_str(),
                    ) {
                        if run.status == RUNNING {
                            runs.push(run);
                        }
                    }
                }
            }
        }
        runs
    }
}

/// Whether `data` was written by a newer version; logs an error if so.
/// Shared with `tools::pending_store` (same state-version contract).
pub(crate) fn is_newer_schema(data: &serde_json::Value, path: &Path) -> bool {
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
        path.display()
    );
    true
}
