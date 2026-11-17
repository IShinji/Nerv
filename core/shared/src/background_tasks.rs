//! File-backed store for gateway-run background tasks (PRD v2 E0).
//!
//! Each task is one JSON file under `personal/background_tasks/<id>.json`,
//! written atomically with a `schema_version` stamp — the same contract as
//! the pending-actions store. The file is the interface: the brain (python)
//! creates and cancels tasks by writing state transitions, and the gateway
//! picks up `QUEUED` entries, runs them in subprocesses, and reports results
//! through targeted notifications. Access is serialized with a file lock.

use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CURRENT_SCHEMA_VERSION: i64 = 1;

/// Task lifecycle states (PRD v2 E0).
pub const QUEUED: &str = "queued";
pub const RUNNING: &str = "running";
pub const DONE: &str = "done";
pub const FAILED: &str = "failed";
pub const CANCELLED: &str = "cancelled";
pub const INTERRUPTED: &str = "interrupted";

/// One background task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTask {
    pub id: String,
    /// What kind of task this is (drives the runner: `command` runs a
    /// subprocess; later kinds map to delegation and scheduled messages).
    pub kind: String,
    /// Runner-specific payload.
    pub payload: Value,
    pub status: String,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub error: String,
    /// Where the completion notification goes.
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub sender: String,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub updated_at: f64,
    #[serde(default)]
    pub schema_version: i64,
}

/// Error type for store operations.
#[derive(Debug, thiserror::Error)]
pub enum TaskStoreError {
    #[error("the state file was written by a newer version of Nerv")]
    NewerSchema,
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub struct BackgroundTaskStore {
    dir: PathBuf,
    lock_path: PathBuf,
}

impl BackgroundTaskStore {
    pub fn new(project_root: &Path) -> Self {
        let dir = project_root.join("personal").join("background_tasks");
        Self {
            lock_path: dir.join(".lock"),
            dir,
        }
    }

    /// Write a task file atomically with the schema stamp (no lock: single
    /// field per file, last-writer-wins with an atomic rename).
    pub fn save(&self, task: &BackgroundTask) -> Result<(), TaskStoreError> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path(&task.id);
        let tmp = self
            .dir
            .join(format!(".{}-{}.tmp", task.id, std::process::id()));
        let mut body =
            serde_json::to_value(task).map_err(|e| TaskStoreError::Io(std::io::Error::other(e)))?;
        if let Some(obj) = body.as_object_mut() {
            obj.insert(
                "schema_version".to_string(),
                serde_json::json!(CURRENT_SCHEMA_VERSION),
            );
        }
        let pretty = serde_json::to_string_pretty(&body)
            .map_err(|e| TaskStoreError::Io(std::io::Error::other(e)))?;
        std::fs::write(&tmp, pretty)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Load one task; `Err(NewerSchema)` when a newer build wrote it.
    pub fn get(&self, id: &str) -> Result<Option<BackgroundTask>, TaskStoreError> {
        let path = self.path(id);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)?;
        let data: Value = serde_json::from_str(&text)
            .map_err(|e| TaskStoreError::Io(std::io::Error::other(e)))?;
        if newer_schema(&data, &path) {
            return Err(TaskStoreError::NewerSchema);
        }
        serde_json::from_value(data)
            .map(Some)
            .map_err(|e| TaskStoreError::Io(std::io::Error::other(e)))
    }

    /// All tasks in a given status (used by the gateway's pickup loop and the
    /// startup sweep).
    pub fn list_by_status(&self, status: &str) -> Vec<BackgroundTask> {
        let mut found = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Some(id) = path.file_stem().map(|s| s.to_string_lossy().to_string()) else {
                    continue;
                };
                match self.get(&id) {
                    Ok(Some(task)) if task.status == status => found.push(task),
                    Ok(Some(task)) if status == QUEUED && task.status == CANCELLED => {
                        // A cancel requested while queued: surface it so the
                        // pickup loop can skip and finalize.
                        found.push(task);
                    }
                    _ => {}
                }
            }
        }
        found.sort_by(|a, b| a.created_at.total_cmp(&b.created_at));
        found
    }

    /// Hold an exclusive lock for a read-modify-write cycle.
    pub fn locked<T>(
        &self,
        body: impl FnOnce() -> Result<T, TaskStoreError>,
    ) -> Result<T, TaskStoreError> {
        std::fs::create_dir_all(&self.dir)?;
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.lock_path)?;
        #[cfg(unix)]
        unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX);
        }
        let result = body();
        #[cfg(unix)]
        unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN);
        }
        result
    }
}

/// Whether `data` was written by a newer build (shared state-version contract).
fn newer_schema(data: &Value, path: &Path) -> bool {
    let version = data
        .get("schema_version")
        .and_then(|v| v.as_i64())
        .unwrap_or(1);
    if version <= CURRENT_SCHEMA_VERSION {
        return false;
    }
    eprintln!(
        "{} has schema_version {version} but this build supports up to \
         {CURRENT_SCHEMA_VERSION}; ignoring it and leaving it untouched",
        path.display()
    );
    true
}

/// A fresh task id: `bg-` + timestamp + a uuid slice.
pub fn new_task_id() -> String {
    let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let token = uuid::Uuid::new_v4().simple().to_string();
    format!("bg-{stamp}-{}", &token[..8])
}
