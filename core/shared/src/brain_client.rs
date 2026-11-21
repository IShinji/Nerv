//! Shared brain IPC types: the targeted-notification message and the error
//! enum used by the brain crates. The supervised sidecar client that used to
//! live here was retired with the python brain — hosts now call
//! `nerv_brain::handle::BrainHandle` directly.

use std::time::Duration;

use thiserror::Error;

/// Errors surfaced while talking to the brain. Kept as the shared shape the
/// brain crates convert their own failures into (hosts match on it).
#[derive(Debug, Error)]
pub enum BrainError {
    #[error("brain process is not running")]
    NotRunning,
    #[error("brain transport failure: {0}")]
    Transport(String),
    #[error("brain call `{method}` timed out after {timeout:?}")]
    Timeout { method: String, timeout: Duration },
    #[error("brain returned an error: {0}")]
    Rpc(String),
    #[error("failed to spawn brain process: {0}")]
    Spawn(String),
}

/// A `notify` notification from the brain: an unsolicited, targeted message
/// meant for one channel/sender (E0 定向通知). The gateway registers a handler
/// that routes it to the right adapter instead of broadcasting.
#[derive(Debug, Clone)]
pub struct NotifyMessage {
    pub channel: String,
    pub sender: String,
    pub text: String,
}
