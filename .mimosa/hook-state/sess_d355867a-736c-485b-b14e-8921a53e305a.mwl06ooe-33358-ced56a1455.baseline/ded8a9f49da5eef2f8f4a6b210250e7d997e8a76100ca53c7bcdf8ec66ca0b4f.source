//! Filesystem tools: read, list, search, and overwrite files.
//! Port of `brain/nerv/tools/files.py`.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use super::helpers::{resolve_tool_path, truncate, MAX_TOOL_OUTPUT_CHARS};
use super::registry::{ToolContext, ToolDefinition, ToolResult};
use super::sandbox::scrubbed_env;
use super::sandbox::{is_secret_path, PROTECTED_FILE_NAMES};

const GREP_TIMEOUT_SECONDS: u64 = 20;

/// Return an error result if `path` is a credentials/config location.
fn deny_secret(path: &str, project_root: &Path) -> Option<ToolResult> {
    if !path.is_empty() && is_secret_path(path, project_root) {
        return Some(ToolResult::error(
            "Error: access to config and secret files is not allowed.",
        ));
    }
    None
}

pub fn file_io() -> ToolDefinition {
    ToolDefinition {
        name: "file_io",
        description: "Inspect files and directories. operation can be read, list, or search. For writes, use write_file_full because writes are higher risk.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "operation": {"type": "string"},
                "path": {"type": "string"},
                "query": {"type": "string"},
                "limit": {"type": "integer"}
            },
            "required": ["operation", "path"]
        }),
        func: |ctx, args| Box::pin(file_io_impl(ctx, args)),
    }
}

async fn file_io_impl(ctx: &ToolContext, args: &Value) -> ToolResult {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let operation = args
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;

    let target = resolve_tool_path(path, &ctx.project_root);
    let target_str = target.display().to_string();
    if let Some(denied) = deny_secret(&target_str, &ctx.project_root) {
        return denied;
    }

    match operation.as_str() {
        "read" => read_file_impl(&target_str),
        "list" => list_dir(&target, limit),
        "search" => {
            if query.trim().is_empty() {
                ToolResult::error("Error: search requires a non-empty query.")
            } else {
                grep_search_impl(&ctx.project_root, &target_str, query).await
            }
        }
        "write" | "append" | "delete" | "move" => ToolResult::error(format!(
            "Unsupported file_io operation '{operation}'. Use write_file_full \
             for writes because write operations require confirmation."
        )),
        _ => ToolResult::error(format!(
            "Unsupported file_io operation '{operation}'. Use read, list, or search."
        )),
    }
}

fn list_dir(target: &std::path::Path, limit: usize) -> ToolResult {
    if !target.exists() {
        return ToolResult::error(format!("Error: Path {} does not exist.", target.display()));
    }
    if !target.is_dir() {
        return ToolResult::error(format!("Error: {} is not a directory.", target.display()));
    }

    let mut entries: Vec<(bool, String)> = std::fs::read_dir(target)
        .map(|rd| {
            rd.flatten()
                .map(|entry| {
                    (
                        entry.path().is_dir(),
                        entry.file_name().to_string_lossy().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    // Directories first, then name (mirrors python's sort key).
    entries.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });

    let shown = entries.len().min(limit.max(1));
    let rendered: Vec<String> = entries[..shown]
        .iter()
        .map(|(is_dir, name)| {
            let kind = if *is_dir { "dir" } else { "file" };
            format!("[{kind}] {name}")
        })
        .collect();

    let mut body = rendered.join("\n");
    if entries.len() > limit {
        body.push_str("\n...[truncated]");
    }
    ToolResult::ok(format!("Listing for {}:\n{body}", target.display()))
}

pub fn read_file() -> ToolDefinition {
    ToolDefinition {
        name: "read_file",
        description: "Read the exact contents of a file at a given absolute path.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "absolute_path": {"type": "string"}
            },
            "required": ["absolute_path"]
        }),
        func: |_ctx, args| {
            let absolute_path = args
                .get("absolute_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Box::pin(async move { read_file_impl(&absolute_path) })
        },
    }
}

fn read_file_impl(absolute_path: &str) -> ToolResult {
    let path = std::path::Path::new(absolute_path);
    if !path.exists() {
        return ToolResult::error(format!("Error: File {absolute_path} does not exist."));
    }
    if !path.is_file() {
        return ToolResult::error(format!("Error: {absolute_path} is not a file."));
    }
    match std::fs::read_to_string(path) {
        Ok(content) => ToolResult::ok(content),
        Err(e) => ToolResult::error(format!("Error reading file: {e}")),
    }
}

pub fn write_file_full() -> ToolDefinition {
    ToolDefinition {
        name: "write_file_full",
        description:
            "Overwrite a file entirely with new content. Use carefully as this deletes old content.",
        // Path-gated by security.sandbox (outside the workspace or protected
        // paths are confirmed), so the blanket flag stays off for ordinary
        // workspace files.
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "absolute_path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["absolute_path", "content"]
        }),
        func: |_ctx, args| {
            let absolute_path = args
                .get("absolute_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Box::pin(async move {
                let path = std::path::Path::new(&absolute_path);
                match path
                    .parent()
                    .map(std::fs::create_dir_all)
                    .unwrap_or(Ok(()))
                    .and_then(|_| std::fs::write(path, &content))
                {
                    Ok(()) => {
                        ToolResult::ok(format!("Successfully completely wrote to {absolute_path}."))
                    }
                    Err(e) => ToolResult::error(format!("Error writing file: {e}")),
                }
            })
        },
    }
}

pub fn grep_search() -> ToolDefinition {
    ToolDefinition {
        name: "grep_search",
        description: "Search for a text pattern inside a directory or file using grep logic.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "search_path": {"type": "string"},
                "query": {"type": "string"}
            },
            "required": ["search_path", "query"]
        }),
        func: |ctx, args| {
            let project_root = ctx.project_root.clone();
            let search_path = args
                .get("search_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Box::pin(async move { grep_search_impl(&project_root, &search_path, &query).await })
        },
    }
}

async fn grep_search_impl(project_root: &Path, search_path: &str, query: &str) -> ToolResult {
    if let Some(denied) = deny_secret(search_path, project_root) {
        return denied;
    }
    let path = std::path::Path::new(search_path);
    if !path.exists() {
        return ToolResult::error(format!("Error: Path {search_path} does not exist."));
    }

    // For MVP we use builtin grep via subprocess. No regex for MVP, just
    // exact match. Never descend into secret locations when searching a
    // parent directory.
    let mut excludes: Vec<String> = vec![
        "--exclude-dir=personal".to_string(),
        "--exclude-dir=.git".to_string(),
    ];
    excludes.extend(
        PROTECTED_FILE_NAMES
            .iter()
            .map(|name| format!("--exclude={name}")),
    );
    let mut command = tokio::process::Command::new("grep");
    command
        .args(["-rnI", "-F"])
        .args(&excludes)
        .arg("--")
        .arg(query)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .envs(scrubbed_env(&[]))
        .kill_on_drop(true);

    let output =
        tokio::time::timeout(Duration::from_secs(GREP_TIMEOUT_SECONDS), command.output()).await;

    match output {
        Ok(Ok(result)) => {
            if result.status.code() == Some(0) {
                let out = truncate(
                    &String::from_utf8_lossy(&result.stdout),
                    MAX_TOOL_OUTPUT_CHARS,
                    "grep_search",
                );
                ToolResult::ok(format!("Found matches:\\n{out}"))
            } else if result.status.code() == Some(1) {
                ToolResult::ok("No matches found.")
            } else {
                ToolResult::error(format!(
                    "Error running search: {}",
                    String::from_utf8_lossy(&result.stderr)
                ))
            }
        }
        Ok(Err(e)) => ToolResult::error(format!("Error searching: {e}")),
        Err(_) => ToolResult::error(format!(
            "Error: search timed out after {GREP_TIMEOUT_SECONDS}s."
        )),
    }
}
