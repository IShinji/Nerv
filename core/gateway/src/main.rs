mod adapter;
mod delegation;
mod ollama;
mod scheduler;
mod tasks_service;

use adapter::cli::CliAdapter;
use adapter::{ChannelAdapter, IncomingMessage};
use anyhow::{Context, Result};
use nerv_brain::handle::BrainHandle;
use nerv_shared::brain_client::NotifyMessage;
use nerv_shared::config::NervConfig;
use nerv_shared::message::Message;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

#[cfg(feature = "email")]
use adapter::email::EmailAdapter;
#[cfg(feature = "slack")]
use adapter::slack::SlackAdapter;
#[cfg(feature = "telegram")]
use adapter::telegram::TelegramAdapter;

/// Determine project root by walking up from the executable location.
fn find_project_root() -> Result<PathBuf> {
    // Try current directory first, then walk up
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join("core").join("config.default.yaml").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }

    // Fallback: try the directory where the binary lives
    let exe = std::env::current_exe()?;
    let mut dir = exe
        .parent()
        .context("cannot determine executable directory")?
        .to_path_buf();
    loop {
        if dir.join("core").join("config.default.yaml").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }

    anyhow::bail!(
        "Cannot find project root (looking for core/config.default.yaml). \
         Run from the project root directory."
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    // The gateway binary doubles as the rust MCP server for the claude-cli
    // bridge: claude spawns the same executable with this flag (see
    // nerv_brain::llm::claude_cli::write_mcp_config).
    if std::env::args().any(|arg| arg == "--mcp-server") {
        nerv_brain::mcp_server::run_stdio().await;
        return Ok(());
    }

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("Nerv gateway starting...");

    // Load configuration
    let project_root = find_project_root()?;
    info!("Project root: {}", project_root.display());

    let config = NervConfig::load(&project_root)?;
    info!("Configuration loaded");
    for warning in config.startup_warnings(&project_root) {
        warn!("{warning}");
    }
    ollama::ensure_ollama_ready(&config).await?;

    // The brain is fully native now — no sidecar to spawn.
    // Targeted notifications (E0): Rust-side events (workflow progress,
    // suspended actions) flow out through this channel into a pump that
    // routes them by channel + sender.
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel::<NotifyMessage>();
    nerv_brain::tools::set_notify_sink(std::sync::Arc::new(move |channel, sender, text| {
        let _ = notify_tx.send(NotifyMessage {
            channel: channel.to_string(),
            sender: sender.to_string(),
            text: text.to_string(),
        });
    }));
    let brain_handle =
        Arc::new(BrainHandle::new(&project_root).context("loading the router prompts")?);
    // Cloned for the two consumers below (the message processor and the
    // scheduler), both of which take ownership.
    let scheduler_brain_handle = Arc::clone(&brain_handle);

    // Create the gateway message channel
    let (gateway_tx, mut gateway_rx) = mpsc::channel::<IncomingMessage>(32);

    // Spawn the message processing loop. Each message gets its own task: the
    // brain multiplexes requests by id, so a slow reply on one channel must not
    // stall every other channel behind it.
    let processor = tokio::spawn(async move {
        while let Some(incoming) = gateway_rx.recv().await {
            let brain_handle = brain_handle.clone();
            tokio::spawn(async move {
                let text = incoming.message.text().unwrap_or("").to_string();
                let channel = incoming.message.channel;
                let sender = incoming.message.sender.clone();

                // Message bodies are private: log only the channel and size.
                info!(
                    "Processing message from {channel} ({} chars)",
                    text.chars().count()
                );

                let response_text = match brain_handle
                    .reply(&text, &channel.to_string(), &sender)
                    .await
                {
                    Ok(result) => {
                        let reply = format_brain_response(&result);
                        info!(
                            "Brain replied on {channel} ({} chars)",
                            reply.chars().count()
                        );
                        reply
                    }
                    Err(e) => {
                        error!("Brain call failed: {e}");
                        format!("Error: {e}")
                    }
                };

                let response = Message::new_text(channel, "nerv", &response_text);

                if incoming.reply_tx.send(response).await.is_err() {
                    error!("Failed to send reply back to adapter");
                }
            });
        }
    });

    // Start the appropriate adapter(s) concurrently!
    let mut adapter_tasks = vec![];
    let mut broadcasters: Vec<Arc<dyn ChannelAdapter>> = Vec::new();

    if config.channels.cli.enabled {
        info!("Starting CLI adapter...");
        let tx_cli = gateway_tx.clone();
        let cli = Arc::new(CliAdapter::new());
        broadcasters.push(cli.clone());
        adapter_tasks.push(tokio::spawn(async move {
            if let Err(e) = cli.start(tx_cli).await {
                error!("CLI adapter crashed: {}", e);
            }
        }));
    }

    #[cfg(feature = "telegram")]
    if config.channels.telegram.enabled && !config.channels.telegram.bot_token.is_empty() {
        // The brain can run shell commands, so an allow-list is mandatory:
        // without one the bot is a remote shell for anyone who finds it.
        if config.channels.telegram.allowed_users.is_empty() {
            error!(
                "Telegram is enabled but channels.telegram.allowed_users is empty. \
                 Refusing to start the adapter — add your Telegram user id to the \
                 allow-list in config.yaml first."
            );
        } else {
            info!(
                "Starting Telegram adapter for {} allowed user(s)...",
                config.channels.telegram.allowed_users.len()
            );
            let tx_tg = gateway_tx.clone();
            let telegram_config = config.channels.telegram.clone();
            let telegram = Arc::new(TelegramAdapter::new(telegram_config, project_root.clone()));
            broadcasters.push(telegram.clone());
            adapter_tasks.push(tokio::spawn(async move {
                if let Err(e) = telegram.start(tx_tg).await {
                    error!("Telegram adapter crashed: {}", e);
                }
            }));
        }
    }

    #[cfg(feature = "email")]
    if config.channels.email.enabled {
        // The brain can run shell commands, so an allow-list is mandatory.
        if config.channels.email.allowed_senders.is_empty() {
            error!(
                "Email is enabled but channels.email.allowed_senders is empty. \
                 Refusing to start the adapter — add your address to the \
                 allow-list in config.yaml first."
            );
        } else {
            info!(
                "Starting Email adapter for {} allowed sender(s)...",
                config.channels.email.allowed_senders.len()
            );
            let tx_mail = gateway_tx.clone();
            let email_config = config.channels.email.clone();
            let email_adapter = Arc::new(EmailAdapter::new(email_config, project_root.clone()));
            broadcasters.push(email_adapter.clone());
            adapter_tasks.push(tokio::spawn(async move {
                if let Err(e) = email_adapter.start(tx_mail).await {
                    error!("Email adapter crashed: {}", e);
                }
            }));
        }
    }

    #[cfg(feature = "slack")]
    if config.channels.slack.enabled {
        // The brain can run shell commands, so an allow-list is mandatory.
        if config.channels.slack.allowed_users.is_empty() {
            error!(
                "Slack is enabled but channels.slack.allowed_users is empty. \
                 Refusing to start the adapter — add your Slack member id to the \
                 allow-list in config.yaml first."
            );
        } else {
            info!(
                "Starting Slack adapter for {} allowed user(s)...",
                config.channels.slack.allowed_users.len()
            );
            let tx_slack = gateway_tx.clone();
            let slack_config = config.channels.slack.clone();
            let slack_adapter = Arc::new(SlackAdapter::new(slack_config));
            broadcasters.push(slack_adapter.clone());
            adapter_tasks.push(tokio::spawn(async move {
                if let Err(e) = slack_adapter.start(tx_slack).await {
                    error!("Slack adapter crashed: {}", e);
                }
            }));
        }
    }

    // Targeted notifications (E0): route each brain `notify` to the matching
    // adapter's per-sender delivery. Unknown channels are dropped with a
    // warning — an unregistered channel must never silently widen delivery.
    let notify_adapters: Arc<std::sync::RwLock<Vec<Arc<dyn ChannelAdapter>>>> =
        Arc::new(std::sync::RwLock::new(broadcasters.clone()));
    let pump_adapters = Arc::clone(&notify_adapters);
    let notify_pump = tokio::spawn(async move {
        while let Some(message) = notify_rx.recv().await {
            // Clone the matching adapter and drop the guard before awaiting:
            // the std guard is not Send and would poison the spawned task.
            let adapter = {
                let adapters = pump_adapters.read().expect("adapters lock");
                adapters
                    .iter()
                    .find(|a| a.channel_type().to_string() == message.channel)
                    .cloned()
            };
            let Some(adapter) = adapter else {
                warn!(
                    "Notification for unknown channel '{}' dropped",
                    message.channel
                );
                continue;
            };
            adapter.send_to(&message.sender, &message.text).await;
        }
    });

    // Scheduled-message scheduler (E2): fires due user-created messages
    // through the normal brain path and delivers replies via targeted notify.
    let scheduler_route: Arc<dyn Fn(String, String, String) + Send + Sync> = {
        let brain_handle = Arc::clone(&scheduler_brain_handle);
        let notify_adapters = Arc::clone(&notify_adapters);
        Arc::new(move |channel, sender, message| {
            let brain_handle = Arc::clone(&brain_handle);
            let adapters = Arc::clone(&notify_adapters);
            tokio::spawn(async move {
                let reply = match brain_handle.reply(&message, &channel, &sender).await {
                    Ok(result) => result
                        .get("reply")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    Err(e) => format!("Error: {e}"),
                };
                // Deliver the reply via targeted notify on the same channel.
                let adapter = {
                    let adapters = adapters.read().expect("adapters lock");
                    adapters
                        .iter()
                        .find(|a| a.channel_type().to_string() == channel)
                        .cloned()
                };
                if let Some(adapter) = adapter {
                    adapter.send_to(&sender, &reply).await;
                }
            });
        })
    };
    let sched_adapters = Arc::clone(&notify_adapters);
    let scheduler = Arc::new(scheduler::Scheduler::new(
        &project_root,
        Arc::new(move |channel, sender, text| {
            let adapters = sched_adapters.read().expect("adapters lock");
            if let Some(adapter) = adapters
                .iter()
                .find(|a| a.channel_type().to_string() == channel)
            {
                let adapter = adapter.clone();
                drop(adapters);
                tokio::spawn(async move {
                    adapter.send_to(&sender, &text).await;
                });
            }
        }),
        scheduler_route,
    ));
    let scheduler = Arc::clone(&scheduler);
    let scheduler_task = scheduler.spawn();

    // Background task service (E0): sweeps INTERRUPTED runs, then polls the
    // shared store for QUEUED tasks and runs them in gateway subprocesses.
    let task_adapters = notify_adapters.clone();
    let task_service = Arc::new(tasks_service::TaskService::new(
        &project_root,
        Arc::new(move |channel, sender, text| {
            let adapters = task_adapters.read().expect("adapters lock");
            if let Some(adapter) = adapters
                .iter()
                .find(|a| a.channel_type().to_string() == channel)
            {
                let adapter = adapter.clone();
                drop(adapters);
                tokio::spawn(async move {
                    adapter.send_to(&sender, &text).await;
                });
            } else {
                warn!("Background task notification for unknown channel '{channel}' dropped");
            }
        }),
    ));
    task_service.sweep_interrupted();
    let tasks_pickup = task_service.clone().spawn_pickup_loop();

    // Heartbeat results go to every channel that can push proactively
    // (the CLI prints them; Telegram messages the allow-listed users). The
    // monitor and the alert queue are native now — no brain round trip.
    let heartbeat_interval = Duration::from_secs(config.brain.heartbeat_interval_secs);
    let heartbeat_task = tokio::spawn(async move {
        let monitor = nerv_brain::proactive::ProactiveMonitor::new(&project_root);
        let mut interval = tokio::time::interval(heartbeat_interval);
        loop {
            interval.tick().await;
            let mut batch = nerv_brain::proactive::drain_notifications();
            batch.extend(monitor.check(None));
            if batch.is_empty() {
                continue;
            }
            let reply = batch.join("\n");
            for adapter in &broadcasters {
                adapter.broadcast(&reply).await;
            }
        }
    });

    // Hang until all running adapters close or crash
    drop(gateway_tx);
    for task in adapter_tasks {
        if let Err(e) = task.await {
            error!("Adapter task spawned failed: {}", e);
        }
    }

    // Cleanup
    heartbeat_task.abort();
    notify_pump.abort();
    tasks_pickup.abort();
    scheduler_task.abort();
    processor.await?;
    info!("Nerv gateway stopped");

    Ok(())
}

/// Format the JSON response from the brain into a user-friendly message.
fn format_brain_response(result: &serde_json::Value) -> String {
    // If the agent produced a reply, that is the answer.
    if let Some(reply) = result.get("reply").and_then(|v| v.as_str()) {
        if !reply.is_empty() {
            return reply.to_string();
        }
    }

    // The brain answered but produced no text — show what it decided so the
    // user is not left with an empty message.
    let intent = result
        .get("intent")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let agent = result
        .get("agent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("general");
    let tier = result
        .get("model_tier")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    format!(
        "📋 Routed to agent={agent} (intent={intent}, tier={tier}), \
         but the agent returned an empty reply."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_brain_response_prefers_the_agent_reply() {
        let result = serde_json::json!({
            "intent": "general",
            "agent_type": "general",
            "model_tier": 0,
            "reply": "Hello!",
        });
        assert_eq!(format_brain_response(&result), "Hello!");
    }

    #[test]
    fn format_brain_response_falls_back_to_the_routing_decision() {
        let result = serde_json::json!({
            "intent": "code_generation",
            "agent_type": "coder",
            "model_tier": 2,
            "reply": "",
        });
        let text = format_brain_response(&result);
        assert!(text.contains("agent=coder"), "{text}");
        assert!(text.contains("tier=2"), "{text}");
        assert!(text.contains("empty reply"), "{text}");
    }

    #[test]
    fn format_brain_response_tolerates_missing_fields() {
        let text = format_brain_response(&serde_json::json!({}));
        assert!(text.contains("agent=general"), "{text}");
    }
}
