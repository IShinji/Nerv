//! Background task service (PRD v2 E0): runs long actions in gateway-owned
//! subprocesses so a brain crash cannot lose them.
//!
//! Tasks live in `personal/background_tasks/<id>.json` (see
//! `nerv_shared::background_tasks`). The brain creates and cancels tasks by
//! writing state transitions to those files; the service polls for `QUEUED`
//! entries, runs each one in its own subprocess, persists results, and
//! reports completion through a targeted `notify`. On gateway startup every
//! `RUNNING` task becomes `INTERRUPTED` with a single notification — runs are
//! never auto-retried (they may have side effects).
//!
//! Only `command` tasks exist for now; delegation (E1) and scheduled sends
//! (E2) plug into the same service.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use nerv_shared::background_tasks::{
    BackgroundTask, BackgroundTaskStore, CANCELLED, DONE, FAILED, INTERRUPTED, QUEUED, RUNNING,
};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tracing::{error, info};

/// How the completion notification reaches the requesting channel/sender.
pub type NotifySender = Arc<dyn Fn(String, String, String) + Send + Sync>;

pub struct TaskService {
    store: BackgroundTaskStore,
    notify: NotifySender,
    /// Live task processes by id, for cancel handling.
    running: Mutex<HashMap<String, tokio::process::Child>>,
}

impl TaskService {
    pub fn new(project_root: &std::path::Path, notify: NotifySender) -> Self {
        Self {
            store: BackgroundTaskStore::new(project_root),
            notify,
            running: Mutex::new(HashMap::new()),
        }
    }

    /// Mark every `RUNNING` task as `INTERRUPTED` (gateway restart) and send
    /// one notification each. Called once at startup; runs are never retried.
    pub fn sweep_interrupted(&self) {
        for mut task in self.store.list_by_status(RUNNING) {
            task.status = INTERRUPTED.to_string();
            task.error = "The gateway restarted while the task was running; \
                          it was interrupted and was not retried."
                .to_string();
            self.persist(&task);
            (self.notify)(
                task.channel.clone(),
                task.sender.clone(),
                format!(
                    "[{}] Task {} was interrupted by a gateway restart and was \
                     not retried.",
                    task.kind, task.id
                ),
            );
        }
    }

    /// The pickup loop: every second, start QUEUED tasks. Runs forever.
    pub fn spawn_pickup_loop(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                for task in self.store.list_by_status(QUEUED) {
                    if let Err(e) = self.spawn_task(task).await {
                        error!("Failed to start background task: {e}");
                    }
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
    }

    /// Start one task's subprocess and a watcher that persists the result.
    async fn spawn_task(self: &Arc<Self>, mut task: BackgroundTask) -> Result<(), std::io::Error> {
        if task.kind == "delegate" {
            // Delegations run via the delegation runner (E1) with the
            // transcript written under personal/delegations/<id>/.
            let service = Arc::clone(self);
            tokio::spawn(async move {
                service.run_delegate_task(task).await;
            });
            return Ok(());
        }
        if task.kind != "command" {
            task.status = FAILED.to_string();
            task.error = format!("Unknown background task kind: {}", task.kind);
            self.persist(&task);
            return Ok(());
        }
        task.status = RUNNING.to_string();
        task.updated_at = now_secs();
        self.persist(&task);
        info!("Background task {} starting", task.id);

        let command = task
            .payload
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let mut child = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        // Accumulate the child's streams in their own tasks so the watcher
        // stays free to poll the cancel flag.
        let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
        let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
        let stdout_task = tokio::spawn(async move {
            let mut buf = String::new();
            let _ = stdout_pipe.read_to_string(&mut buf).await;
            buf
        });
        let stderr_task = tokio::spawn(async move {
            let mut buf = String::new();
            let _ = stderr_pipe.read_to_string(&mut buf).await;
            buf
        });

        let mut guard = self.running.lock().await;
        guard.insert(task.id.clone(), child);
        drop(guard);

        let service = Arc::clone(self);
        let id = task.id.clone();
        tokio::spawn(async move {
            service.watch_task(id, stdout_task, stderr_task).await;
        });
        Ok(())
    }

    /// Watch one running task: await its exit (or a CANCELLED flag in the
    /// file — kill the process), persist the outcome, deliver the targeted
    /// notification, and remove the process entry.
    async fn watch_task(
        self: Arc<Self>,
        id: String,
        stdout_task: tokio::task::JoinHandle<String>,
        stderr_task: tokio::task::JoinHandle<String>,
    ) {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;

            // A cancel flips the file status; kill the process when seen.
            let cancel_requested = matches!(
                self.store.get(&id),
                Ok(Some(task)) if task.status == CANCELLED
            );

            let mut guard = self.running.lock().await;
            let Some(child) = guard.get_mut(&id) else {
                return; // already reaped
            };
            match child.try_wait() {
                Ok(Some(status)) => {
                    guard.remove(&id);
                    drop(guard);

                    let stdout = stdout_task.await.unwrap_or_default();
                    let stderr = stderr_task.await.unwrap_or_default();
                    let mut task = match self.store.get(&id) {
                        Ok(Some(task)) => task,
                        _ => return,
                    };
                    // A cancel requested while running wins over the exit.
                    let cancelled = cancel_requested || task.status == CANCELLED;
                    if cancelled {
                        task.status = CANCELLED.to_string();
                        self.persist(&task);
                        (self.notify)(
                            task.channel.clone(),
                            task.sender.clone(),
                            format!("[{}] Task {} was cancelled.", task.kind, task.id),
                        );
                        return;
                    }
                    task.status = if status.success() {
                        DONE.to_string()
                    } else {
                        FAILED.to_string()
                    };
                    task.result = stdout;
                    task.error = stderr;
                    self.persist(&task);

                    let summary = if task.status == DONE {
                        format!(
                            "[{}] Task {} finished.{}",
                            task.kind,
                            task.id,
                            if task.result.is_empty() {
                                String::new()
                            } else {
                                format!(" Result: {}", task.result)
                            }
                        )
                    } else {
                        format!("[{}] Task {} failed: {}", task.kind, task.id, task.error)
                    };
                    (self.notify)(task.channel.clone(), task.sender.clone(), summary);
                    return;
                }
                Ok(None) => {
                    if cancel_requested {
                        let _ = child.kill().await;
                        // The next poll sees the exit and finalizes.
                    }
                }
                Err(e) => {
                    error!("Background task {id} wait failed: {e}");
                    guard.remove(&id);
                    return;
                }
            }
        }
    }

    /// Run a delegation task end to end and report via targeted notify.
    async fn run_delegate_task(self: Arc<Self>, task: BackgroundTask) {
        let outcome = super::delegation::run_delegate(&task).await;
        (self.notify)(task.channel.clone(), task.sender.clone(), outcome);
    }

    fn persist(&self, task: &BackgroundTask) {
        if let Err(e) = self.store.save(task) {
            error!("Failed to persist background task {}: {e}", task.id);
        }
    }
}

use std::time::{SystemTime, UNIX_EPOCH};

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
