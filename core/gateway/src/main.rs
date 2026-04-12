mod adapter;
mod brain_client;
mod ollama;

use adapter::cli::CliAdapter;
use adapter::{ChannelAdapter, IncomingMessage};
use anyhow::{Context, Result};
use brain_client::{find_brain_dir, BrainClient};
use nerv_shared::config::NervConfig;
use nerv_shared::message::Message;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info};

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
    let brain_dir = find_brain_dir(&project_root);
    let brain = Arc::new(BrainClient::new(
        &config.brain.python_command,
        &config.brain.module,
        &brain_dir,
        &config.models.router.model,
        &config.models.router.base_url,
        &config.models.tiers.tier0,
        &config.models.tiers.tier1,
        &config.models.tiers.tier2,
        &config.models.tiers.tier3,
        config.brain.restart_max,
    ));
    brain.start().await?;

    // Create the gateway message channel
    let (gateway_tx, mut gateway_rx) = mpsc::channel::<IncomingMessage>(32);

    // Spawn the message processing loop
    let brain_clone = brain.clone();
    let processor = tokio::spawn(async move {
        while let Some(incoming) = gateway_rx.recv().await {
            let text = incoming.message.text().unwrap_or("").to_string();
            let channel = incoming.message.channel;

            info!("Processing message from {channel}: {text}");

            // Call the Python brain for routing
            let params = serde_json::json!({
                "message": text,
                "channel": channel.to_string(),
                "sender": incoming.message.sender,
            });

            let response_text = match brain_clone.call("route", params).await {
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
        }
    });

    // Start heartbeat loop (Priority 4)
    let brain_heartbeat = brain.clone();
    let heartbeat_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            if let Ok(result) = brain_heartbeat
                .call("heartbeat", serde_json::json!({}))
                .await
            {
                if let Some(reply) = result.get("reply").and_then(|v| v.as_str()) {
                    if !reply.is_empty() {
                        // Print the proactive background hook message to the screen!
                        println!("\n{reply}\n");
                    }
                }
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
        info!("Starting Telegram adapter...");
        let tx_tg = gateway_tx.clone();
        let token = config.channels.telegram.bot_token.clone();
        adapter_tasks.push(tokio::spawn(async move {
            let telegram = TelegramAdapter::new(token);
            if let Err(e) = telegram.start(tx_tg).await {
                error!("Telegram adapter crashed: {}", e);
            }
        }));
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
    // If the router provided a direct reply, use it
    if let Some(reply) = result.get("reply").and_then(|v| v.as_str()) {
        if !reply.is_empty() {
            return reply.to_string();
        }
    }

    // Fallback: show the routing result
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
        "📋 Classified: intent={intent}, agent={agent}, tier={tier}\n\
         (Full agent execution not implemented yet — this is the router result)"
    )
}
