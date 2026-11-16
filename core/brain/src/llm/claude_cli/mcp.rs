//! The MCP bridge that exposes Nerv's native tools to `claude -p`: the temp
//! config file pointing Claude at the Nerv MCP server, and the capability →
//! provider tool-name mapping. Port of `_write_mcp_config` /
//! `_resolve_provider_names` in `brain/nerv/llm/claude_cli.py` and the alias
//! table from `brain/nerv/capabilities/registry.py`.

use std::path::{Path, PathBuf};

use serde_json::json;

/// Prefix of the temp MCP config written for each request (the Python sweep
/// in `nerv.mcp_server` startup cleans leftovers of both implementations).
pub const MCP_SERVER_NAME: &str = "nerv";
pub const MCP_CONFIG_PREFIX: &str = "nerv-mcp-";

/// Map requested tool/capability names to concrete provider tool names.
///
/// Python resolves through the capability registry: a direct tool name
/// resolves to itself, a capability alias resolves to its provider, and an
/// unknown name passes through unchanged — the static alias table below plus
/// pass-through is exactly equivalent.
pub fn resolve_provider_names(tool_names: &[String]) -> Vec<String> {
    let mut resolved: Vec<String> = Vec::new();
    for name in tool_names {
        let provider = capability_alias(name).unwrap_or(name);
        if !resolved.iter().any(|r| r == provider) {
            resolved.push(provider.to_string());
        }
    }
    resolved
}

fn capability_alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "browser.read" => "browser",
        "browser.interactive" => "browser_interactive",
        "filesystem.read" => "file_io",
        "filesystem.read_exact" => "read_file",
        "local.exec" => "shell",
        "calendar.add" => "calendar_add",
        "calendar.read" => "calendar_list",
        "notes.write" => "note_write",
        "notes.read" => "note_read",
        "email.send" => "send_email",
        "agent.evolve" => "evolve_agent",
        "agent.versions" => "list_agent_versions",
        "agent.rollback" => "rollback_agent",
        _ => return None,
    })
}

/// Write a temp MCP config that points Claude at the Nerv tool server.
///
/// Returns the path; the caller removes it when the request finishes (a
/// `finally` in Python). The file must be removed before the process exits —
/// a brain killed mid-request leaves it behind, and the sidecar's startup
/// sweep deletes files older than an hour with this prefix.
pub fn write_mcp_config(project_root: &Path, scope: &str) -> std::io::Result<PathBuf> {
    // Memory tools in the MCP process must read this request's scope, not the
    // default one.
    let mut env = json!({
        "NERV_PROJECT_ROOT": project_root.to_string_lossy(),
        "NERV_MEMORY_SCOPE": scope,
    });
    // The MCP server is a separate process with an explicit environment. Hand
    // it the user config file this process resolved, so its sandbox settings
    // (security.workspace_root) match the orchestrator's whatever its own HOME
    // or NERV_CONFIG are.
    if let Some(user_config) = nerv_shared::config::resolve_user_config_path(
        project_root,
        std::env::var_os("NERV_CONFIG").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    ) {
        // Python's Path.resolve() also works on missing paths, so fall back to
        // the plain path when canonicalize fails.
        let resolved = user_config.canonicalize().unwrap_or(user_config);
        env["NERV_CONFIG"] = json!(resolved.to_string_lossy());
    }
    let config = json!({
        "mcpServers": {
            MCP_SERVER_NAME: {
                "command": "uv",
                "args": ["run", "python", "-m", "nerv.mcp_server"],
                "cwd": project_root.join("brain").to_string_lossy(),
                "env": env,
            }
        }
    });

    let path = std::env::temp_dir().join(format!(
        "{MCP_CONFIG_PREFIX}{}-{}.json",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::write(&path, serde_json::to_string(&config).unwrap())?;
    Ok(path)
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
