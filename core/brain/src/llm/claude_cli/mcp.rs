//! The MCP bridge that exposes Nerv's native tools to `claude -p`: the temp
//! config file pointing Claude at the Nerv MCP server, and the capability →
//! provider tool-name mapping. Port of `_write_mcp_config` /
//! `_resolve_provider_names` in `brain/nerv/llm/claude_cli.py` and the alias
//! table from `brain/nerv/capabilities/registry.py`.

use std::path::{Path, PathBuf};

use serde_json::json;

/// Prefix of the temp MCP config written for each request.
pub const MCP_SERVER_NAME: &str = "nerv";
pub const MCP_CONFIG_PREFIX: &str = "nerv-mcp-";
/// A temp MCP config older than this is a leftover from a brain killed
/// mid-request (the file is removed in a `finally` that never ran).
pub const STALE_MCP_CONFIG_SECONDS: f64 = 3600.0;

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

/// Write a temp MCP config that points Claude at the Nerv tool servers: the
/// rust registry (this binary, `--mcp-server` mode) serves the ported tools,
/// the python sidecar serves the rest. Returns the path; the caller removes
/// it when the request finishes (a `finally` in python).
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
    // The rust server is the same binary that launched the turn, in its
    // --mcp-server mode (works for the gateway and the GUI alike).
    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("nerv"));

    sweep_stale_mcp_configs(None);

    let config = json!({
        "mcpServers": {
            MCP_SERVER_NAME: {
                "command": current_exe.display().to_string(),
                "args": ["--mcp-server"],
                "env": env,
            },
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

/// Delete temp MCP configs left behind by a brain that was killed
/// mid-request. Only our own regular files with the Nerv prefix older than
/// [`STALE_MCP_CONFIG_SECONDS`] are removed, so a live request's file, a
/// symlink or anyone else's file is never touched. Returns how many were
/// removed.
pub fn sweep_stale_mcp_configs(directory: Option<&Path>) -> usize {
    use std::time::UNIX_EPOCH;

    let dir = directory
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let cutoff = now - STALE_MCP_CONFIG_SECONDS;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut removed = 0;
    let mut paths: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(MCP_CONFIG_PREFIX))
        {
            continue;
        }
        // lstat semantics: a symlink is judged as itself, never followed.
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.uid() != unsafe { libc::getuid() } {
                continue;
            }
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(f64::INFINITY);
        if mtime < cutoff && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
