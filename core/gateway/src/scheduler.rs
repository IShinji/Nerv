//! Scheduled-message scheduler (PRD v2 E2).
//!
//! Polls the shared store for active messages whose `next_fire` is due,
//! sends each one as a regular user message through the brain (so the
//! normal route → agent → reply path runs), delivers the reply to the
//! message's channel/sender via targeted notify, and recomputes the next
//! fire for recurring messages.
//!
//! "Only the most recent due": the gateway compares `next_fire <= now` —
//! if the machine slept for days, the past-due message fires once and the
//! next occurrence is computed from *now*, not from the missed schedule.

use std::sync::Arc;
use std::time::Duration;

use tracing::info;

use crate::tasks_service::NotifySender;
use nerv_shared::scheduled_messages::{is_due, next_occurrence, ScheduledMessageStore};

pub struct Scheduler {
    store: ScheduledMessageStore,
    #[allow(dead_code)]
    notify: NotifySender,
    /// Sends `message` as a user message from `channel`/`sender` through the
    /// brain, and delivers the reply via targeted notify.
    pub route_message: Arc<dyn Fn(String, String, String) + Send + Sync>,
}

impl Scheduler {
    pub fn new(
        store_dir: &std::path::Path,
        notify: NotifySender,
        route_message: Arc<dyn Fn(String, String, String) + Send + Sync>,
    ) -> Self {
        Self {
            store: ScheduledMessageStore::new(store_dir),
            notify,
            route_message,
        }
    }

    /// The scheduler loop: every 30 seconds, check for due messages.
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                self.poll().await;
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        })
    }

    async fn poll(&self) {
        let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        for message in self.store.list_active() {
            if !is_due(&message.next_fire, &now) {
                continue;
            }

            // Send the message through the normal brain path.
            let route = Arc::clone(&self.route_message);
            let channel = message.channel.clone();
            let sender = message.sender.clone();
            let text = message.message.clone();
            tokio::spawn(async move {
                route(channel, sender, text);
            });

            // Recurring: recompute next_fire from now (not from the missed
            // schedule, per the "only most recent due" rule).
            let next = next_occurrence(&now, &message.recurrence);
            match next {
                Some(next) => {
                    let mut updated = message.clone();
                    updated.next_fire = next.clone();
                    let _ = self.store.save(&updated);
                    info!(
                        "Scheduled message {} fired; next fire at {next}",
                        message.id
                    );
                }
                None => {
                    // One-shot: cancel after firing.
                    let mut done = message.clone();
                    done.status = nerv_shared::scheduled_messages::CANCELLED.to_string();
                    let _ = self.store.save(&done);
                    info!("Scheduled message {} fired (one-shot)", message.id);
                }
            }
        }
    }
}
