//! Builtin tools the rust brain executes natively. The registry, the
//! sandbox policy and the shared helpers are ports of
//! `brain/nerv/tools/` (which keeps running for the MCP server until that
//! migrates too).

pub mod delegation;
pub mod desktop;
pub mod evolution;
pub mod files;
pub mod helpers;
pub mod knowledge;
pub mod memory;
pub mod pending_store;
pub mod productivity;
pub mod registry;
pub mod sandbox;
pub mod system;
pub mod web;
pub mod workflow_exec;

#[cfg(test)]
mod tests;

pub use registry::{ToolContext, ToolDefinition, ToolRegistry, ToolResult};

/// The targeted-notify sink: set once by the gateway so Rust-side events
/// (workflow progress) reach the requesting channel (E0 定向通知).
static NOTIFY_SINK: std::sync::OnceLock<crate::workflows::executor::NotifyFn> =
    std::sync::OnceLock::new();

pub fn set_notify_sink(sink: crate::workflows::executor::NotifyFn) {
    let _ = NOTIFY_SINK.set(sink);
}

pub fn notify_sink() -> Option<crate::workflows::executor::NotifyFn> {
    NOTIFY_SINK.get().cloned()
}

/// The sandbox root for the facade: config override, else the project root.
pub fn facade_workspace_root(project_root: &Path) -> PathBuf {
    nerv_shared::config::NervConfig::load(project_root)
        .map(|config| config.security.workspace_root(project_root))
        .unwrap_or_else(|_| {
            project_root
                .canonicalize()
                .unwrap_or(project_root.to_path_buf())
        })
}

/// The pending-record summary the confirmation flows render (same text as
/// python's `_format_pending_action`, implemented in turn/pending.rs).
pub fn pending_store_format(record: &crate::turn::PendingRecord) -> String {
    crate::turn::format_pending_action(record)
}

use std::path::{Path, PathBuf};
