//! Slack adapter — Socket Mode for inbound events, chat.postMessage for
//! replies (PRD v2 2.4, SlackAdapter). Feature-gated behind `slack`.
//!
//! Authorization mirrors the Telegram adapter and fails closed: only
//! `allowed_users` (Slack member ids) reach the brain.

use std::time::Duration;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use nerv_shared::config::SlackChannelConfig;
use nerv_shared::message::{ChannelType, Message};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{error, info, warn};

use super::IncomingMessage;

/// The Socket Mode websocket endpoint.
const SOCKET_MODE_URL: &str = "wss://slack.com/socket/";
/// The Web API endpoint for replies.
const WEB_API_URL: &str = "https://slack.com/api/chat.postMessage";
/// Socket Mode requires acknowledging every envelope within 3 s.
const ACK_TIMEOUT: Duration = Duration::from_secs(3);

pub struct SlackAdapter {
    config: SlackChannelConfig,
}

impl SlackAdapter {
    pub fn new(config: SlackChannelConfig) -> Self {
        Self { config }
    }
}

/// Extract one inbound message (user id + text) from a Socket Mode envelope's
/// `event`, tolerating message-subtypes (edits, joins) by skipping them.
fn parse_event(event: &serde_json::Value) -> Option<(String, String)> {
    let event_type = event.get("type").and_then(|v| v.as_str())?;
    if event_type != "message" {
        return None;
    }
    // Edits carry a `subtype` (message_changed etc.) — only bare messages count.
    if event.get("subtype").is_some() {
        return None;
    }
    let user = event.get("user").and_then(|v| v.as_str())?;
    let text = event.get("text").and_then(|v| v.as_str())?;
    if text.trim().is_empty() {
        return None;
    }
    Some((user.to_string(), text.to_string()))
}

/// The JSON payload for a Socket Mode ACK of an envelope id.
fn ack_payload(envelope_id: &str) -> WsMessage {
    WsMessage::text(json!({"envelope_id": envelope_id}).to_string())
}

/// Post one message to a channel via the Slack Web API.
async fn web_post_message(bot_token: &str, channel: &str, text: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let response = client
        .post(WEB_API_URL)
        .bearer_auth(bot_token)
        .json(&json!({"channel": channel, "text": text}))
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    let body: serde_json::Value = response.json().await?;
    if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let error = body.get("error").and_then(|v| v.as_str()).unwrap_or("?");
        anyhow::bail!("slack chat.postMessage failed: {error}");
    }
    Ok(())
}

#[async_trait::async_trait]
impl super::ChannelAdapter for SlackAdapter {
    async fn start(&self, gateway_tx: mpsc::Sender<IncomingMessage>) -> Result<()> {
        info!("Slack adapter started (Socket Mode)");
        let config = self.config.clone();

        loop {
            // Connect: Socket Mode requires a wss url with the app token.
            let request = SOCKET_MODE_URL.into_client_request()?;
            let request = {
                let mut request = request;
                request.headers_mut().insert(
                    "Authorization",
                    format!("Bearer {}", config.app_token)
                        .parse()
                        .expect("header value"),
                );
                request
            };
            let (mut ws, _response) = tokio_tungstenite::connect_async(request).await?;
            info!("Slack Socket Mode connected");

            loop {
                let Some(msg) = ws.next().await else {
                    break; // connection closed; reconnect
                };
                let Ok(WsMessage::Text(raw)) = msg else {
                    continue;
                };
                let Ok(envelope) = serde_json::from_str::<serde_json::Value>(&raw) else {
                    warn!("Invalid JSON from Slack Socket Mode");
                    continue;
                };
                // ACK first — Slack drops the app if acks are late.
                let Some(envelope_id) = envelope.get("envelope_id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let ack = ack_payload(envelope_id);
                if tokio::time::timeout(ACK_TIMEOUT, ws.send(ack))
                    .await
                    .is_err()
                {
                    error!("Slack Socket Mode ack timed out; reconnecting");
                    break;
                }

                let Some(payload) = envelope.get("payload") else {
                    continue;
                };
                let Some(event) = payload.get("event") else {
                    continue;
                };
                let Some((user, text)) = parse_event(event) else {
                    continue;
                };
                if !config.is_allowed(&user) {
                    warn!("Rejected Slack message from unauthorized sender {user}");
                    continue;
                }
                let _channel_id = event
                    .get("channel")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let message = Message::new_text(ChannelType::Slack, &user, &text);
                let (reply_tx, _reply_rx) = mpsc::channel::<Message>(1);
                if gateway_tx
                    .send(IncomingMessage {
                        message,
                        reply_tx,
                        progress_tx: None,
                    })
                    .await
                    .is_err()
                {
                    error!("Gateway channel closed");
                    return Ok(());
                }
            }
            warn!("Slack Socket Mode disconnected; reconnecting in 5s");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    async fn stop(&self) -> Result<()> {
        Ok(())
    }

    /// Replies go to the Slack channel the message came from (`sender` here
    /// carries the `channel_id` set by the notify pump caller when known;
    /// the pump passes the sender id, so keep a best-effort DM by user id).
    async fn send_to(&self, sender: &str, text: &str) {
        // chat.postMessage to the user id opens an IM channel when the bot
        // can reach that user; that is the normal notify path.
        if let Err(e) = web_post_message(&self.config.bot_token, sender, text).await {
            error!("Failed to send Slack reply to {sender}: {e}");
        }
    }

    fn channel_type(&self) -> ChannelType {
        ChannelType::Slack
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_parse_only_bare_messages() {
        let event = json!({"type": "message", "user": "U123", "text": "hello", "channel": "C1"});
        assert_eq!(
            parse_event(&event),
            Some(("U123".to_string(), "hello".to_string()))
        );

        // Edits and joins are skipped.
        let edit = json!({"type": "message", "subtype": "message_changed", "user": "U123"});
        assert_eq!(parse_event(&edit), None);
        let join = json!({"type": "message", "subtype": "channel_join", "user": "U123"});
        assert_eq!(parse_event(&join), None);
        // Non-message events are skipped.
        let reaction = json!({"type": "reaction_added", "user": "U123"});
        assert_eq!(parse_event(&reaction), None);
        // Empty text is skipped.
        let empty = json!({"type": "message", "user": "U123", "text": "  "});
        assert_eq!(parse_event(&empty), None);
    }

    #[test]
    fn ack_payloads_name_the_envelope() {
        let ack = ack_payload("env-1");
        if let WsMessage::Text(text) = ack {
            assert!(text.contains("env-1"), "{text}");
            assert!(text.contains("envelope_id"), "{text}");
        } else {
            panic!("expected a text frame");
        }
    }

    #[test]
    fn authorization_fails_closed() {
        let cfg = SlackChannelConfig {
            allowed_users: vec!["U123".to_string()],
            ..Default::default()
        };
        assert!(cfg.is_allowed("U123"));
        assert!(!cfg.is_allowed("U999"));
        assert!(!cfg.is_allowed(""));
        assert!(!SlackChannelConfig::default().is_allowed("U123"));
    }
}
