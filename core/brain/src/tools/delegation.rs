//! Delegation tools (PRD v2 E1): hand a coding task to a native coding-agent
//! CLI (Claude Code / Codex), running it as a gateway background task.
//!
//! The gate (auto-execute vs confirm) mirrors the PRD boundary rules: inside
//! the sandbox, read-only or isolated-worktree edit, budget/time within caps
//! → run; anything else queues a pending action with the reason.

use serde_json::{json, Value};

use super::registry::{ToolContext, ToolDefinition, ToolResult};
use crate::tools::pending_store::PendingActionStore;
use crate::tools::sandbox;

/// The delegation parameters, validated and normalized.
struct DelegationParams {
    executor_name: String,
    task: String,
    workspace: String,
    mode: String,
    justification: String,
    max_budget_usd: f64,
    timeout_s: u64,
}

fn parse_arguments(args: &Value) -> Result<DelegationParams, ToolResult> {
    let get = |key: &str| {
        args.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let executor_name = get("executor");
    let task = get("task");
    let workspace = get("workspace");
    let mode = get("mode");
    let justification = get("justification");
    let max_budget_usd = args
        .get("max_budget_usd")
        .and_then(|v| v.as_f64())
        .unwrap_or(2.0);
    let timeout_s = args
        .get("timeout_s")
        .and_then(|v| v.as_u64())
        .unwrap_or(1800);

    if executor_name.trim().is_empty() {
        return Err(ToolResult::error(
            "Error: delegate_task requires `executor` (one of the configured executors).",
        ));
    }
    if task.trim().is_empty() {
        return Err(ToolResult::error("Error: delegate_task requires `task`."));
    }
    if workspace.trim().is_empty() {
        return Err(ToolResult::error(
            "Error: delegate_task requires `workspace`.",
        ));
    }
    if mode != "read_only" && mode != "edit" {
        return Err(ToolResult::error(format!(
            "Error: mode must be 'read_only' or 'edit' (got {mode:?})."
        )));
    }
    if max_budget_usd <= 0.0 {
        return Err(ToolResult::error("Error: max_budget_usd must be positive."));
    }
    if timeout_s == 0 {
        return Err(ToolResult::error("Error: timeout_s must be positive."));
    }
    Ok(DelegationParams {
        executor_name,
        task,
        workspace,
        mode,
        justification,
        max_budget_usd,
        timeout_s,
    })
}

/// The external task id for one delegation.
fn delegation_task_id(prefix: &str) -> String {
    let stamp = chrono::Local::now().format("%Y%m%d%H%M%S");
    let token: String = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    format!("{prefix}-{stamp}-{token}")
}

pub fn delegate_task() -> ToolDefinition {
    ToolDefinition {
        name: "delegate_task",
        description: "Hand a coding task to a native coding-agent CLI (Claude Code or Codex). It runs in the background in an isolated workspace and reports the result when done. Args: executor, task, workspace, mode (read_only|edit), justification, max_budget_usd, timeout_s.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "executor": {"type": "string"},
                "task": {"type": "string"},
                "workspace": {"type": "string"},
                "mode": {"type": "string"},
                "justification": {"type": "string"},
                "max_budget_usd": {"type": "number"},
                "timeout_s": {"type": "integer"}
            },
            "required": ["executor", "task", "workspace"]
        }),
        func: |ctx, args| {
            let ctx = ToolContext {
                project_root: ctx.project_root.clone(),
                workspace_root: ctx.workspace_root.clone(),
                scope: ctx.scope.clone(),
                brain: ctx.brain.clone(),
            };
            let args = args.clone();
            Box::pin(async move { delegate_task_impl(&ctx, &args) })
        },
    }
}

fn delegate_task_impl(ctx: &ToolContext, args: &Value) -> ToolResult {
    let params = match parse_arguments(args) {
        Ok(params) => params,
        Err(reply) => return reply,
    };

    let config = match nerv_shared::config::NervConfig::load(&ctx.project_root) {
        Ok(config) => config.delegation,
        Err(e) => return ToolResult::error(format!("Failed to load config: {e}")),
    };

    // The executor must be one of the configured ones.
    let Some(executor) = config
        .executors
        .iter()
        .find(|e| e.name == params.executor_name)
    else {
        let names = config
            .executors
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let available = if names.is_empty() {
            "(none configured — add delegation.executors in config)".to_string()
        } else {
            names
        };
        return ToolResult::error(format!(
            "Error: unknown executor {:?}. Available: {available}",
            params.executor_name
        ));
    };

    // Hard caps: the config's ceilings override whatever the model asked for.
    let budget = params.max_budget_usd.min(config.max_budget_usd);
    let timeout = params.timeout_s.min(config.max_timeout_s);

    // Resolve the workspace and apply the boundary gate (PRD v2 E1: inside
    // the sandbox and not protected → run; anything else → confirm).
    let workspace = super::helpers::resolve_tool_path(&params.workspace, &ctx.project_root);
    let workspace_str = workspace.display().to_string();
    let nervs_own_repo =
        ctx.project_root.join(".git").exists() && workspace.starts_with(&ctx.project_root);

    if workspace_str.is_empty() {
        return ToolResult::error("Error: workspace resolution failed.");
    }
    let is_within = sandbox::is_within_sandbox(&workspace_str, &ctx.workspace_root);
    let is_protected = sandbox::is_protected_path(&workspace_str, &ctx.project_root);
    let gate_reason = if !is_within {
        Some(format!(
            "workspace '{workspace_str}' is outside the Nerv workspace sandbox"
        ))
    } else if is_protected {
        Some(format!(
            "workspace '{workspace_str}' is a protected config/secret/code location"
        ))
    } else if nervs_own_repo && params.mode == "edit" {
        // A delegation into Nerv's own repository must run in an isolated
        // worktree so the main checkout can never be modified directly.
        Some("editing Nerv's own repository requires an isolated git worktree".to_string())
    } else {
        None
    };

    // Out of bounds: queue a pending action with the reason.
    if let Some(reason) = gate_reason {
        let store = PendingActionStore::new(&ctx.project_root);
        let record = match store.add(
            "delegate_task",
            json!({
                "executor": params.executor_name,
                "task": params.task,
                "workspace": workspace_str,
                "mode": params.mode,
                "max_budget_usd": budget,
                "timeout_s": timeout,
            }),
            &reason,
            "",
            "",
            &ctx.scope,
            &params.justification,
        ) {
            Ok(record) => record,
            Err(e) => return ToolResult::error(format!("Failed to queue the delegation: {e}")),
        };
        return ToolResult::error(format!(
            "[ACTION SUSPENDED #{}] {reason}. This delegation was NOT started. The \
             user must approve it in Nerv with 'confirm {}' before it runs. Do not \
             retry — tell the user what is pending.",
            record.id, record.id
        ));
    }

    // In bounds: create the background task and return the id.
    let task_id = delegation_task_id(&params.executor_name);
    let task = nerv_shared::background_tasks::BackgroundTask {
        id: task_id.clone(),
        kind: "delegate".to_string(),
        payload: json!({
            "executor": params.executor_name,
            "executor_kind": executor.kind,
            "mode": params.mode,
            "workspace": workspace_str,
            "task": params.task,
            "max_budget_usd": budget,
            "timeout_s": timeout,
        }),
        status: nerv_shared::background_tasks::QUEUED.to_string(),
        result: String::new(),
        error: String::new(),
        channel: String::new(),
        sender: String::new(),
        created_at: 0.0,
        updated_at: 0.0,
        schema_version: 1,
    };
    let store = nerv_shared::background_tasks::BackgroundTaskStore::new(&ctx.project_root);
    if let Err(e) = store.save(&task) {
        return ToolResult::error(format!("Failed to create the background task: {e}"));
    }

    ToolResult::ok(format!(
        "Delegation started: task {task_id}. The {} agent is working on it in {} \
         mode (budget ${budget:.2}, timeout {timeout}s). You will be notified when \
         it finishes; ask for its status with 'delegation_status {task_id}' or \
         cancel it with 'cancel {task_id}'.",
        params.executor_name, params.mode
    ))
}

pub fn delegation_status() -> ToolDefinition {
    ToolDefinition {
        name: "delegation_status",
        description: "Check the status of a background delegation. Args: task_id.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "task_id": {"type": "string"}
            },
            "required": ["task_id"]
        }),
        func: |ctx, args| {
            let task_id = args
                .get("task_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let store = nerv_shared::background_tasks::BackgroundTaskStore::new(&ctx.project_root);
            Box::pin(async move {
                match store.get(&task_id) {
                    Ok(Some(task)) => ToolResult::ok(format!(
                        "[{}] Task {}: {}{}",
                        task.kind,
                        task.id,
                        task.status,
                        if task.result.is_empty() {
                            String::new()
                        } else {
                            format!(" — {}", task.result)
                        }
                    )),
                    Ok(None) => ToolResult::error(format!("No task with id {task_id}.")),
                    Err(e) => ToolResult::error(format!("Failed to read the task: {e}")),
                }
            })
        },
    }
}
