//! Builtin tools the rust brain executes natively. The registry, the
//! sandbox policy and the shared helpers are ports of
//! `brain/nerv/tools/` (which keeps running for the MCP server until that
//! migrates too).

pub mod files;
pub mod helpers;
pub mod pending_store;
pub mod registry;
pub mod sandbox;
pub mod system;

#[cfg(test)]
mod tests;

pub use registry::{ToolContext, ToolDefinition, ToolRegistry, ToolResult};

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
