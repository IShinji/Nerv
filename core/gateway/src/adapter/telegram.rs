use anyhow::Result;
use nerv_shared::message::{ChannelType, Message};
use teloxide::prelude::*;
use teloxide::types::Message as TgMessage;
use tokio::sync::mpsc;
use tracing::{error, info};

use super::IncomingMessage;

/// Telegram adapter — communicates with Telegram Bot API via teloxide.
pub struct TelegramAdapter {
    bot_token: String,
}

impl TelegramAdapter {
    pub fn new(bot_token: String) -> Self {
        Self { bot_token }
    }
}

#[async_trait::async_trait]
impl super::ChannelAdapter for TelegramAdapter {
    async fn start(&self, gateway_tx: mpsc::Sender<IncomingMessage>) -> Result<()> {
        info!("Telegram adapter started");

        let bot = Bot::new(&self.bot_token);

        teloxide::repl(bot, move |bot: Bot, msg: TgMessage| {
            let gateway_tx = gateway_tx.clone();
            async move {
                let text = msg.text().unwrap_or("").to_string();
                if text.is_empty() {
                    return Ok(());
                }

                let chat_id = msg.chat.id;
                let sender = chat_id.0.to_string();

                let message = Message::new_text(ChannelType::Telegram, &sender, &text);
                let (reply_tx, mut reply_rx) = mpsc::channel::<Message>(1);

                let incoming = IncomingMessage { message, reply_tx };

                if gateway_tx.send(incoming).await.is_err() {
                    error!("Gateway channel closed");
                    return Ok(());
                }

                // Show thinking indicator
                let _ = bot
                    .send_chat_action(chat_id, teloxide::types::ChatAction::Typing)
                    .await;

                // Wait for the brain's response and send it back to Telegram
                if let Some(response) = reply_rx.recv().await {
                    if let Some(response_text) = response.text() {
                        let mut final_text = response_text.to_string();
                        
                        // Parse all occurrences of [DOCUMENT: path]
                        while let Some(start) = final_text.find("[DOCUMENT: ") {
                            if let Some(end_offset) = final_text[start..].find(']') {
                                let end = start + end_offset;
                                // We extract the file path between [DOCUMENT: and ]
                                let path_str = final_text[start + 11..end].trim();
                                let path = std::path::Path::new(path_str);
                                
                                if path.exists() {
                                    let input_file = teloxide::types::InputFile::file(path);
                                    match bot.send_document(chat_id, input_file).await {
                                        Ok(_) => info!("Sent document to Telegram: {}", path_str),
                                        Err(e) => error!("Failed to send document: {}", e),
                                    }
                                } else {
                                    error!("Requested document does not exist: {}", path_str);
                                }
                                
                                // Remove the token from the text
                                let mut cleaned = final_text[..start].to_string();
                                cleaned.push_str(&final_text[end + 1..]);
                                final_text = cleaned.trim().to_string();
                            } else {
                                // Malformed token, break to avoid infinite loop
                                break;
                            }
                        }

                        if !final_text.is_empty() {
                            match bot.send_message(chat_id, final_text).await {
                                Ok(_) => info!("Sent reply to Telegram"),
                                Err(e) => error!("Failed to send reply to Telegram: {}", e),
                            }
                        }
                    }
                }

                Ok(())
            }
        })
        .await;

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        info!("Telegram adapter stopped");
        Ok(())
    }

    fn channel_type(&self) -> ChannelType {
        ChannelType::Telegram
    }
}
