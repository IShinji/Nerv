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
#[async_trait::async_trait]
pub trait ChannelAdapter: Send + Sync {
    /// Start listening for incoming messages.
    /// Received messages should be sent via `gateway_tx`.
    async fn start(&self, gateway_tx: mpsc::Sender<IncomingMessage>) -> Result<()>;

    /// Stop the adapter gracefully.
    async fn stop(&self) -> Result<()>;

    /// Identify which channel this adapter serves.
    fn channel_type(&self) -> ChannelType;
}
