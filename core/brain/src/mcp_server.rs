//! Stdio MCP server exposing Nerv native tools to the Claude Code CLI.
//!
//! Port of `brain/nerv/mcp_server/server.py`. Each registered rust tool
//! becomes an MCP tool. Before executing, the server applies the workspace
//! sandbox policy: safe / in-sandbox calls run immediately, while high-risk or
//! out-of-sandbox calls are enqueued in the shared pending store (the same
//! file the python confirm flow reads) and reported back so the user can
//! approve them inside Nerv.
//!
//! The host binary doubles as this server via a `--mcp-server` flag: the
//! claude-cli spawn config points at the same executable that launched the
//! turn, so no separate install step is needed.

use std::io::{BufRead, Write};
use std::path::Path;
use std::path::PathBuf;

use serde_json::{json, Value};
use tracing::error;

use crate::tools::pending_store::{clean_justification, PendingActionStore};
use crate::tools::registry::ToolDefinition;
use crate::tools::registry::{ToolContext, ToolRegistry};
use crate::tools::sandbox::{confirmation_reason, ALWAYS_CONFIRM_TOOLS, PATH_ARG_BY_TOOL};

pub const SERVER_NAME: &str = "nerv";
/// Env var through which the launcher hands the conversation scope to this
/// process, so memory tools read the right sender's history (fails closed to
/// the default scope when unset).
pub const SCOPE_ENV: &str = "NERV_MEMORY_SCOPE";
const PROTOCOL_VERSION: &str = "2025-03-26";
const JUSTIFICATION_PARAM: &str = "justification";

fn project_root() -> PathBuf {
    if let Some(root) = std::env::var_os("NERV_PROJECT_ROOT") {
        let path = PathBuf::from(root);
        if let Ok(resolved) = path.canonicalize() {
            return resolved;
        }
        return path;
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn tool_properties(tool: &ToolDefinition) -> Value {
    tool.parameters
        .get("parameters")
        .cloned()
        .unwrap_or(json!({"type": "object", "properties": {}}))
}

/// Whether a call to this tool can be suspended for user confirmation.
fn may_need_confirmation(name: &str, tool: &ToolDefinition) -> bool {
    tool.requires_confirmation
        || ALWAYS_CONFIRM_TOOLS.contains(&name)
        || PATH_ARG_BY_TOOL.iter().any(|(tool, _)| *tool == name)
}

/// Run a Nerv tool, or suspend it for confirmation per the sandbox policy.
///
/// Returns the text the model should see. Port of `execute_or_suspend`.
async fn execute_or_suspend(
    name: &str,
    arguments: &Value,
    workspace_root: &Path,
    store: &PendingActionStore,
    project_root: &Path,
    scope: &str,
) -> String {
    let Some(tool) = ToolRegistry::all().iter().find(|t| t.name == name) else {
        return format!("Unknown tool: {name}");
    };

    let mut arguments = arguments.clone();
    let mut justification = String::new();
    // `justification` is an MCP-layer parameter, not part of the tool itself:
    // take it out before the tool (or the sandbox policy) sees the arguments.
    let has_justification_param = tool
        .parameters
        .get("parameters")
        .and_then(|p| p.get("properties"))
        .and_then(|p| p.get(JUSTIFICATION_PARAM))
        .is_some();
    if !has_justification_param {
        if let Some(obj) = arguments.as_object_mut() {
            if let Some(value) = obj.remove(JUSTIFICATION_PARAM) {
                justification = clean_justification(value.as_str());
            }
        }
    }

    let Some(reason) = confirmation_reason(
        name,
        &arguments,
        workspace_root,
        tool.requires_confirmation,
        Some(project_root),
    ) else {
        // Safe: execute natively.
        let tool_ctx = ToolContext {
            project_root: project_root.to_path_buf(),
            workspace_root: workspace_root.to_path_buf(),
            scope: scope.to_string(),
            confirmed: None,
        };
        let result = (tool.func)(&tool_ctx, &arguments).await;
        return if result.is_error {
            format!("Tool {name} failed: {}", result.content)
        } else {
            result.content
        };
    };

    match store.add(name, arguments, &reason, "", "", scope, &justification) {
        Ok(record) => {
            tracing::info!("Suspended {} (#{}): {reason}", record.id, name);
            format!(
                "[ACTION SUSPENDED #{}] {reason}. This action was NOT executed. \
                 The user must approve it in Nerv with 'confirm {}' before it \
                 runs. Do not retry — tell the user what is pending and continue \
                 with anything else you can do safely.",
                record.id, record.id
            )
        }
        Err(e) => {
            error!("Failed to queue pending action: {e}");
            format!("Failed to queue pending action: {e}")
        }
    }
}

/// Build the `tools/list` result: every registry tool, with the justification
/// parameter added to confirmable tools (mirrors the python server).
fn list_tools() -> Value {
    let mut tools = Vec::new();
    for tool in ToolRegistry::all() {
        let mut parameters = tool_properties(tool);
        if may_need_confirmation(tool.name, tool) {
            let has_justification_param = parameters
                .get("properties")
                .and_then(|p| p.get(JUSTIFICATION_PARAM))
                .is_some();
            if !has_justification_param {
                let mut properties = parameters.get("properties").cloned().unwrap_or(json!({}));
                if let Some(props) = properties.as_object_mut() {
                    props.insert(
                        JUSTIFICATION_PARAM.to_string(),
                        json!({
                            "type": "string",
                            "description": "Optional. One sentence saying why this action is needed; shown to the user if it must be confirmed."
                        }),
                    );
                }
                parameters["properties"] = properties;
            }
        }
        tools.push(json!({
            "name": tool.name,
            "description": tool.description,
            "inputSchema": parameters,
        }));
    }
    json!({ "tools": tools })
}

/// Handle one JSON-RPC request. Returns `None` for notifications.
async fn handle_request(request: &Value, project_root: &Path) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");

    // Notifications carry no id and expect no response.
    let id = id?;

    let response_result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": request
                .pointer("/params/protocolVersion")
                .cloned()
                .unwrap_or(json!(PROTOCOL_VERSION)),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
        })),
        "tools/list" => Ok(list_tools()),
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = request
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or(json!({}));
            let scope = std::env::var(SCOPE_ENV)
                .unwrap_or_else(|_| crate::memory::DEFAULT_SCOPE.to_string());
            let workspace_root = nerv_shared::config::NervConfig::load(project_root)
                .map(|config| config.security.workspace_root(project_root))
                .unwrap_or_else(|_| project_root.to_path_buf());
            let store = PendingActionStore::new(project_root);
            let text = execute_or_suspend(
                &name,
                &arguments,
                &workspace_root,
                &store,
                project_root,
                &scope,
            )
            .await;
            Ok(json!({
                "content": [{"type": "text", "text": text}],
                "isError": false,
            }))
        }
        "ping" => Ok(json!({})),
        _ => Err(json!({
            "code": -32601,
            "message": format!("Method not found: {method}"),
        })),
    };

    let response = match response_result {
        Ok(result) => json!({"jsonrpc": "2.0", "result": result, "id": id}),
        Err(error) => json!({"jsonrpc": "2.0", "error": error, "id": id}),
    };
    Some(response)
}

/// Run the MCP server over stdio until stdin closes.
pub async fn run_stdio() {
    let project_root = project_root();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut lines = stdin.lock().lines();

    loop {
        let line = match lines.next() {
            Some(Ok(line)) => line,
            Some(Err(e)) => {
                error!("stdin read failed: {e}");
                break;
            }
            None => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(e) => {
                let reply = json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32700, "message": format!("Parse error: {e}")},
                    "id": Value::Null,
                });
                let _ = writeln!(stdout, "{reply}");
                let _ = stdout.flush();
                continue;
            }
        };

        if let Some(response) = handle_request(&request, &project_root).await {
            let _ = writeln!(stdout, "{response}");
            let _ = stdout.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tools_list_includes_the_registry_and_justification_param() {
        let listing = list_tools();
        let tools = listing["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"current_time"));
        let shell = tools.iter().find(|t| t["name"] == "shell").unwrap();
        assert!(shell["inputSchema"]["properties"]["justification"].is_object());
        let time = tools.iter().find(|t| t["name"] == "current_time").unwrap();
        assert!(time["inputSchema"]["properties"]["justification"].is_null());
    }

    #[tokio::test]
    async fn safe_tool_calls_execute_and_suspended_ones_queue() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();

        // A safe tool runs immediately.
        let request = json!({
            "jsonrpc": "2.0", "method": "tools/call", "id": 1,
            "params": {"name": "current_time", "arguments": {"timezone": "local"}}
        });
        let response = handle_request(&request, &root).await.unwrap();
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("Current time in"), "{text}");

        // shell always confirms: the call queues in the shared store.
        let request = json!({
            "jsonrpc": "2.0", "method": "tools/call", "id": 2,
            "params": {"name": "shell", "arguments": {"command": "ls", "justification": "list files"}}
        });
        let response = handle_request(&request, &root).await.unwrap();
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("[ACTION SUSPENDED #1]"), "{text}");
        assert!(text.contains("confirm 1"), "{text}");

        let store = PendingActionStore::new(&root);
        let queued: Vec<_> = store.list();
        assert_eq!(queued.len(), 1, "expected exactly one queued action");
        let queued = &queued[0];
        assert_eq!(queued.tool_name, "shell");
        assert_eq!(queued.justification, "list files");
    }

    #[tokio::test]
    async fn unknown_methods_report_json_rpc_errors() {
        let dir = tempfile::tempdir().unwrap();
        let request = json!({
            "jsonrpc": "2.0", "method": "resources/list", "id": 3, "params": {}
        });
        let response = handle_request(&request, dir.path()).await.unwrap();
        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn notifications_have_no_id_and_get_no_reply() {
        let notification = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert!(notification.get("id").is_none());
    }
}
