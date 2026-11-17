mod adapter;
mod delegation;
mod ollama;
mod tasks_service;

use adapter::cli::CliAdapter;
use adapter::{ChannelAdapter, IncomingMessage};
use anyhow::{Context, Result};
use nerv_brain::handle::BrainHandle;
use nerv_shared::brain_client::{find_brain_dir, BrainClient, BrainSpawnConfig, NotifyMessage};
use nerv_shared::config::NervConfig;
use nerv_shared::message::Message;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

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

/// Build the brain spawn config from the loaded Nerv config.
fn brain_spawn_config(config: &NervConfig, project_root: &std::path::Path) -> BrainSpawnConfig {
    let brain_dir = find_brain_dir(project_root);

    let mut spawn = BrainSpawnConfig::python(
        &config.brain.python_command,
        &config.brain.module,
        &brain_dir,
    )
    .with_restart_max(config.brain.restart_max)
    .with_call_timeout(Duration::from_secs(config.brain.call_timeout_secs))
    .with_env("NERV_PROJECT_ROOT", project_root.to_string_lossy())
    .with_env("NERV_OLLAMA_URL", &config.models.router.base_url)
    .with_env("NERV_MODEL_TIER0", &config.models.tiers.tier0)
    .with_env("NERV_MODEL_TIER1", &config.models.tiers.tier1)
    .with_env("NERV_MODEL_TIER2", &config.models.tiers.tier2)
    .with_env("NERV_MODEL_TIER3", &config.models.tiers.tier3);

    // Keep uv's cache inside the project unless the user chose their own.
    if std::env::var_os("UV_CACHE_DIR").is_none() {
        spawn = spawn.with_env(
            "UV_CACHE_DIR",
            brain_dir.join(".uv-cache").to_string_lossy(),
        );
    }

    spawn
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

    // Start the Python brain process, then the Rust brain facade: the router
    // classifies locally, the python sidecar handles pending/execute.
    let brain = Arc::new(BrainClient::new(brain_spawn_config(&config, &project_root)));
    // Targeted notifications (E0): brain `notify` messages flow out through
    // this channel into a pump that routes them by channel + sender.
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel::<NotifyMessage>();
    brain.set_notify_handler(notify_tx);
    brain.start().await?;
    let brain_handle = Arc::new(
        BrainHandle::new(brain.clone(), &project_root).context("loading the router prompts")?,
    );

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
    // (the CLI prints them; Telegram messages the allow-listed users).
    let brain_heartbeat = brain.clone();
    let heartbeat_interval = Duration::from_secs(config.brain.heartbeat_interval_secs);
    let heartbeat_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(heartbeat_interval);
        loop {
            interval.tick().await;
            match brain_heartbeat
                .call("heartbeat", serde_json::json!({}))
                .await
            {
                Ok(result) => {
                    if let Some(reply) = result.get("reply").and_then(|v| v.as_str()) {
                        if !reply.is_empty() {
                            for adapter in &broadcasters {
                                adapter.broadcast(reply).await;
                            }
                        }
                    }
                }
                Err(e) => warn!("Heartbeat failed: {e}"),
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
    processor.await?;
    brain.stop().await?;
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
