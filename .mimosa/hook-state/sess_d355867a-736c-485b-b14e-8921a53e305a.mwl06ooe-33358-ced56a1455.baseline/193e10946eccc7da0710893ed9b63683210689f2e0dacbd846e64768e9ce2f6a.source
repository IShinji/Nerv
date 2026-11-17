//! Chat memory — the port of `brain/nerv/memory/`: per-scope conversation
//! JSONL stores, the shared `facts.md`, the token-budgeted prompt assembly
//! and the rolling day summaries.
//!
//! The python `MemoryManager` survives for the MCP server's memory tools
//! (they read and write the same files through the same lock protocol — the
//! on-disk format is the interface between the two implementations).

pub mod context;
pub mod manager;
pub mod models;
pub mod summarizer;

#[cfg(test)]
mod tests;

pub use context::{estimate_tokens as context_estimate_tokens, AgentPrompt, ContextManager};
pub use manager::{MemoryManager, SearchHit, DEFAULT_SCOPE};
pub use models::{DailyConversation, StoredChatMessage};
pub use summarizer::{backfill_summaries, ArcCaller};
