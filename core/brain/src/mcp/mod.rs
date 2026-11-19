//! MCP client/manager — the port of `brain/nerv/mcp/` (E9d).
//!
//! The manager owns the configured MCP server processes (stdio JSON-RPC),
//! resolves capability bindings, and answers the `browser.interactive`
//! capability's MCP-first path natively; python's `nerv.mcp` is retired
//! alongside it.

pub mod client;
pub mod manager;
pub mod presets;

#[cfg(test)]
mod tests;

pub use client::{McpError, McpServerConfig, StdioMcpClient, DEFAULT_MCP_PROTOCOL_VERSION};
pub use manager::{get_mcp_manager, McpServerManager, McpStatusEntry};
pub use presets::{get_builtin_mcp_preset, list_builtin_mcp_presets, render_mcp_preset_snippet};
