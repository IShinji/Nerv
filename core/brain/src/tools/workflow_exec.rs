//! The `execute_workflow` tool: run a shared workflow through the native
//! executor, with the run-level approval flow (a browser workflow is queued
//! for the user's approval as a whole, not once per step).
//!
//! Replaces the python `knowledge.execute_workflow` + `workflows/executor.py`.

use serde_json::{json, Value};

use super::registry::{ToolContext, ToolDefinition, ToolResult};
use crate::turn::model_loop::ProviderCaller;
use crate::workflows::executor::{ExecutorContext, WorkflowExecutor};

pub fn execute_workflow() -> ToolDefinition {
    ToolDefinition {
        name: "execute_workflow",
        description: "Execute a named shared workflow. This delegates control to the Workflow Executor engine which will sequentially perform the steps and return the final structurally extracted response. A workflow that drives the browser (or another tool that needs confirmation) is first queued for the user's approval as a whole: tell the user to confirm it instead of retrying.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "workflow_name": {"type": "string"},
                "inputs": {"type": "string"}
            },
            "required": ["workflow_name", "inputs"]
        }),
        func: |ctx, args| {
            let ctx = ToolContext {
                project_root: ctx.project_root.clone(),
                workspace_root: ctx.workspace_root.clone(),
                scope: ctx.scope.clone(),
                confirmed: ctx.confirmed.clone(),
            };
            let args = args.clone();
            Box::pin(async move { execute_workflow_impl(&ctx, &args).await })
        },
    }
}

async fn execute_workflow_impl(ctx: &ToolContext, args: &Value) -> ToolResult {
    let Some(workflow_name) = args.get("workflow_name").and_then(|v| v.as_str()) else {
        return ToolResult::error("Error: execute_workflow requires `workflow_name`.");
    };
    let inputs = args.get("inputs").and_then(|v| v.as_str()).unwrap_or("");

    let registry = crate::workflows::WorkflowRegistry::new(&ctx.project_root);
    let Some(workflow) = registry.find(workflow_name.trim()) else {
        return ToolResult::error(format!(
            "Error: Shared workflow '{workflow_name}' not found. List workflows first."
        ));
    };
    if workflow.steps.is_empty() {
        return ToolResult::error(format!(
            "Error: Workflow '{}' has no defined steps.",
            workflow.name
        ));
    }

    // Model help for argument extraction and reasoning steps (tier 0) —
    // served natively by the llm client.
    let caller = std::sync::Arc::new(ProviderCaller {
        claude_cli_backend: crate::llm::claude_cli::ClaudeCliBackend::default(),
        project_root: ctx.project_root.clone(),
    });

    // The scope slug is channel-sender; best-effort split for targeting.
    let (channel, sender) = split_scope(&ctx.scope);
    let executor_ctx = ExecutorContext {
        project_root: ctx.project_root.clone(),
        workspace_root: ctx.workspace_root.clone(),
        scope: ctx.scope.clone(),
        channel,
        sender,
        notify: crate::tools::notify_sink(),
        confirmed: ctx.confirmed.clone(),
        caller,
    };
    let mut executor = WorkflowExecutor::new(executor_ctx);

    match executor.execute(&workflow, inputs, None).await {
        Ok(final_result) => ToolResult::ok(format!(
            "Workflow executed successfully.\nResult context:\n{final_result}"
        )),
        Err(pending) => ToolResult {
            content: pending.message,
            is_error: pending.stale,
            is_pending: false,
        },
    }
}

/// Split the scope slug back into (channel, sender) for targeted notify.
fn split_scope(scope: &str) -> (String, String) {
    match scope.split_once('-') {
        Some((channel, sender)) if !channel.is_empty() && !sender.is_empty() => {
            (channel.to_string(), sender.to_string())
        }
        _ => (scope.to_string(), scope.to_string()),
    }
}
