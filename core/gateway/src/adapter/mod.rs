pub mod cli;
#[cfg(feature = "telegram")]
pub mod telegram;

use anyhow::Result;
use nerv_shared::message::{ChannelType, Message};
use tokio::sync::mpsc;

/// Incoming message from an adapter, ready for the gateway to process.
#[derive(Debug)]
pub struct IncomingMessage {
    pub message: Message,
    /// Channel to send the response back through the originating adapter.
    pub reply_tx: mpsc::Sender<Message>,
}

/// Channel adapter trait — each messaging channel implements this interface.
///
/// The adapter converts channel-specific messages to/from the unified Message
/// format and communicates with the gateway via mpsc channels.
// async_trait expands the default `broadcast` body into a `#[must_use]` future
// that clippy flags as redundant; it is not something we can fix at the source.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait ChannelAdapter: Send + Sync {
    /// Start listening for incoming messages.
    /// Received messages should be sent via `gateway_tx`.
    async fn start(&self, gateway_tx: mpsc::Sender<IncomingMessage>) -> Result<()>;

    /// Stop the adapter gracefully.
    #[allow(dead_code)]
    async fn stop(&self) -> Result<()>;

    /// Push an unsolicited message (e.g. a heartbeat result) to this channel's
    /// authorized recipients. Channels that cannot do so keep the no-op default.
    async fn broadcast(&self, _text: &str) {}

    /// Identify which channel this adapter serves.
    #[allow(dead_code)]
    fn channel_type(&self) -> ChannelType;
}
