mod adapter;
mod ollama;

use adapter::cli::CliAdapter;
use adapter::{ChannelAdapter, IncomingMessage};
use anyhow::{Context, Result};
use nerv_shared::brain_client::{find_brain_dir, BrainClient, BrainSpawnConfig};
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
    .with_env("NERV_ROUTER_MODEL", &config.models.router.model)
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
    ollama::ensure_ollama_ready(&config).await?;

    // Start the Python brain process
    let brain = Arc::new(BrainClient::new(brain_spawn_config(&config, &project_root)));
    brain.start().await?;

    // Create the gateway message channel
    let (gateway_tx, mut gateway_rx) = mpsc::channel::<IncomingMessage>(32);

    // Spawn the message processing loop. Each message gets its own task: the
    // brain multiplexes requests by id, so a slow reply on one channel must not
    // stall every other channel behind it.
    let brain_clone = brain.clone();
    let processor = tokio::spawn(async move {
        while let Some(incoming) = gateway_rx.recv().await {
            let brain = brain_clone.clone();
            tokio::spawn(async move {
                let text = incoming.message.text().unwrap_or("").to_string();
                let channel = incoming.message.channel;

                info!("Processing message from {channel}: {text}");

                let params = serde_json::json!({
                    "message": text,
                    "channel": channel.to_string(),
                    "sender": incoming.message.sender,
                });

                let response_text = match brain.call("route", params).await {
                    Ok(result) => {
                        info!("Brain response: {result}");
                        format_brain_response(&result)
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

    // Start heartbeat loop (Priority 4)
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
                            // Print the proactive background hook message to the screen!
                            println!("\n{reply}\n");
                        }
                    }
                }
                Err(e) => warn!("Heartbeat failed: {e}"),
            }
        }
    });

    // Start the appropriate adapter(s) concurrently!
    let mut adapter_tasks = vec![];

    if config.channels.cli.enabled {
        info!("Starting CLI adapter...");
        let tx_cli = gateway_tx.clone();
        adapter_tasks.push(tokio::spawn(async move {
            let cli = CliAdapter::new();
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
            adapter_tasks.push(tokio::spawn(async move {
                let telegram = TelegramAdapter::new(telegram_config);
                if let Err(e) = telegram.start(tx_tg).await {
                    error!("Telegram adapter crashed: {}", e);
                }
            }));
        }
    }

    // Hang until all running adapters close or crash
    drop(gateway_tx);
    for task in adapter_tasks {
        if let Err(e) = task.await {
            error!("Adapter task spawned failed: {}", e);
        }
    }

    // Cleanup
    heartbeat_task.abort();
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
