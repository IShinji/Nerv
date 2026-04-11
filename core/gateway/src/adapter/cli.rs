use anyhow::Result;
use nerv_shared::message::{ChannelType, Message};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::IncomingMessage;

/// CLI adapter — reads from stdin, writes to stdout.
/// Used for development and debugging.
#[derive(Default)]
pub struct CliAdapter;

impl CliAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl super::ChannelAdapter for CliAdapter {
    async fn start(&self, gateway_tx: mpsc::Sender<IncomingMessage>) -> Result<()> {
        info!("CLI adapter started. Type a message and press Enter. Type 'quit' to exit.");

        let stdin = tokio::io::stdin();
        let reader = BufReader::new(stdin);
        let mut lines = reader.lines();

        // Each incoming message gets a reply channel so the gateway can send responses back
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            if line == "quit" || line == "exit" {
                info!("CLI adapter: user requested exit");
                break;
            }

            let message = Message::new_text(ChannelType::Cli, "cli_user", &line);
            let (reply_tx, mut reply_rx) = mpsc::channel::<Message>(1);

            let incoming = IncomingMessage { message, reply_tx };

            if gateway_tx.send(incoming).await.is_err() {
                warn!("Gateway channel closed, stopping CLI adapter");
                break;
            }

            // Wait for the response and print it
            if let Some(response) = reply_rx.recv().await {
                if let Some(text) = response.text() {
                    println!("\n🤖 Nerv: {text}\n");
                }
            }
        }

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        info!("CLI adapter stopped");
        Ok(())
    }

    fn channel_type(&self) -> ChannelType {
        ChannelType::Cli
    }
}
