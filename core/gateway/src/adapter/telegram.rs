use anyhow::Result;
use nerv_shared::config::TelegramChannelConfig;
use nerv_shared::message::{ChannelType, Message};
use teloxide::prelude::*;
use teloxide::types::Message as TgMessage;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use super::IncomingMessage;

/// Marker Nerv emits to have the gateway upload a file to the channel.
const DOCUMENT_TAG: &str = "[DOCUMENT: ";

/// Telegram adapter — communicates with Telegram Bot API via teloxide.
pub struct TelegramAdapter {
    config: TelegramChannelConfig,
}

impl TelegramAdapter {
    pub fn new(config: TelegramChannelConfig) -> Self {
        Self { config }
    }
}

/// A path Nerv asked to be uploaded, plus the text with the marker removed.
struct Document {
    path: String,
    remaining_text: String,
}

/// Pull the next `[DOCUMENT: path]` marker out of `text`, if there is one.
fn take_document(text: &str) -> Option<Document> {
    let start = text.find(DOCUMENT_TAG)?;
    let end = start + text[start..].find(']')?;
    let path = text[start + DOCUMENT_TAG.len()..end].trim().to_string();

    let mut remaining = text[..start].to_string();
    remaining.push_str(&text[end + 1..]);

    Some(Document {
        path,
        remaining_text: remaining.trim().to_string(),
    })
}

#[async_trait::async_trait]
impl super::ChannelAdapter for TelegramAdapter {
    async fn start(&self, gateway_tx: mpsc::Sender<IncomingMessage>) -> Result<()> {
        info!("Telegram adapter started");

        let bot = Bot::new(&self.config.bot_token);
        let config = self.config.clone();

        teloxide::repl(bot, move |bot: Bot, msg: TgMessage| {
            let gateway_tx = gateway_tx.clone();
            let config = config.clone();
            async move {
                let text = msg.text().unwrap_or("").to_string();
                if text.is_empty() {
                    return Ok(());
                }

                let chat_id = msg.chat.id;
                let sender = chat_id.0.to_string();

                // Authorize before the message can reach the brain, which can
                // run shell commands and read the filesystem. Unknown senders
                // get no reply at all — an error message would confirm that a
                // Nerv instance is listening on this bot.
                if !config.is_allowed(&sender) {
                    warn!("Rejected Telegram message from unauthorized sender {sender}");
                    return Ok(());
                }

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

                        while let Some(document) = take_document(&final_text) {
                            let path = std::path::Path::new(&document.path);
                            if path.exists() {
                                let input_file = teloxide::types::InputFile::file(path);
                                match bot.send_document(chat_id, input_file).await {
                                    Ok(_) => info!("Sent document to Telegram: {}", document.path),
                                    Err(e) => error!("Failed to send document: {}", e),
                                }
                            } else {
                                error!("Requested document does not exist: {}", document.path);
                            }
                            final_text = document.remaining_text;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_document_extracts_the_path_and_strips_the_marker() {
        let doc = take_document("Here you go [DOCUMENT: /tmp/report.pdf] enjoy").unwrap();
        assert_eq!(doc.path, "/tmp/report.pdf");
        assert_eq!(doc.remaining_text, "Here you go  enjoy");
    }

    #[test]
    fn take_document_returns_none_without_a_marker() {
        assert!(take_document("just text").is_none());
    }

    #[test]
    fn take_document_returns_none_for_an_unterminated_marker() {
        // Regression guard: the loop that consumes these must terminate.
        assert!(take_document("oops [DOCUMENT: /tmp/x").is_none());
    }

    #[test]
    fn take_document_consumes_every_marker_in_turn() {
        let mut text = "a [DOCUMENT: /one] b [DOCUMENT: /two] c".to_string();
        let mut paths = Vec::new();
        while let Some(doc) = take_document(&text) {
            paths.push(doc.path);
            text = doc.remaining_text;
        }
        assert_eq!(paths, vec!["/one", "/two"]);
        assert!(!text.contains("DOCUMENT"));
    }

    #[test]
    fn only_allow_listed_senders_are_accepted() {
        let adapter = TelegramAdapter::new(TelegramChannelConfig {
            enabled: true,
            bot_token: "token".to_string(),
            allowed_users: vec!["42".to_string()],
        });

        assert!(adapter.config.is_allowed("42"));
        assert!(!adapter.config.is_allowed("43"));
    }
}
