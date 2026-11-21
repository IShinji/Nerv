//! Delegation tools (PRD v2 E1): hand a coding task to a native coding-agent
//! CLI (Claude Code / Codex), running it as a gateway background task.
//!
//! The gate (auto-execute vs confirm) mirrors the PRD boundary rules: inside
//! the sandbox, read-only or isolated-worktree edit, budget/time within caps
//! → run; anything else queues a pending action with the reason.

use std::path::Path;

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
            confirmed: None,
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

    // In bounds: queue the background task and return the id.
    match enqueue_delegation(
        &ctx.project_root,
        &params.executor_name,
        &executor.kind,
        &params.mode,
        &params.task,
        &workspace_str,
        budget,
        timeout,
    ) {
        Ok(task_id) => ToolResult::ok(format!(
            "Delegation started: task {task_id}. The {} agent is working on it in {} \
             mode (budget ${budget:.2}, timeout {timeout}s). You will be notified when \
             it finishes; ask for its status with 'delegation_status {task_id}' or \
             cancel it with 'cancel {task_id}'.",
            params.executor_name, params.mode
        )),
        Err(e) => ToolResult::error(format!("Failed to create the background task: {e}")),
    }
}

/// Queue one delegation background task. Shared by `delegate_task` and the
/// parallel fan-out (`dispatch_agents`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn enqueue_delegation(
    project_root: &Path,
    executor_name: &str,
    executor_kind: &str,
    mode: &str,
    task: &str,
    workspace: &str,
    budget: f64,
    timeout: u64,
) -> Result<String, String> {
    let task_id = delegation_task_id(executor_name);
    let task = nerv_shared::background_tasks::BackgroundTask {
        id: task_id.clone(),
        kind: "delegate".to_string(),
        payload: json!({
            "executor": executor_name,
            "executor_kind": executor_kind,
            "mode": mode,
            "workspace": workspace,
            "task": task,
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
    let store = nerv_shared::background_tasks::BackgroundTaskStore::new(project_root);
    store
        .save(&task)
        .map(|_| task_id)
        .map_err(|e| e.to_string())
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

/// Parallel fan-out (PRD Phase 2: 复杂任务图并行调度): hand the same task to
/// several executors at once, each running in its own background task, and
/// report every task id so the caller can track them.
pub fn dispatch_agents() -> ToolDefinition {
    ToolDefinition {
        name: "dispatch_agents",
        description: "Fan a coding task out to multiple executors in parallel (complex-task graph scheduling). Each executor works in its own background task and reports independently. Args: executors (array of executor names), task, workspace, mode (read_only|edit).",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "executors": {"type": "array", "items": {"type": "string"}},
                "task": {"type": "string"},
                "workspace": {"type": "string"},
                "mode": {"type": "string"}
            },
            "required": ["executors", "task", "workspace"]
        }),
        func: |ctx, args| {
            let ctx = ToolContext {
                project_root: ctx.project_root.clone(),
                workspace_root: ctx.workspace_root.clone(),
                scope: ctx.scope.clone(),
                confirmed: ctx.confirmed.clone(),
            };
            let args = args.clone();
            Box::pin(async move { dispatch_agents_impl(&ctx, &args).await })
        },
    }
}

async fn dispatch_agents_impl(ctx: &ToolContext, args: &Value) -> ToolResult {
    let Some(executors) = args.get("executors").and_then(|v| v.as_array()) else {
        return ToolResult::error("Error: dispatch_agents requires `executors` (an array).");
    };
    let Some(task) = args.get("task").and_then(|v| v.as_str()) else {
        return ToolResult::error("Error: dispatch_agents requires `task`.");
    };
    let Some(workspace) = args.get("workspace").and_then(|v| v.as_str()) else {
        return ToolResult::error("Error: dispatch_agents requires `workspace`.");
    };
    let mode = args
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("read_only");
    let names: Vec<String> = executors
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    if names.is_empty() {
        return ToolResult::error("Error: `executors` must name at least one executor.");
    }

    let config = match nerv_shared::config::NervConfig::load(&ctx.project_root) {
        Ok(config) => config.delegation,
        Err(e) => return ToolResult::error(format!("Failed to load config: {e}")),
    };

    // The user's whole-fan-out approval (a confirmed pending action) clears
    // the workspace gate below — one approval covers every executor.
    let approved = ctx
        .confirmed
        .as_ref()
        .is_some_and(|c| c.tool_name == "dispatch_agents" && c.arguments == *args);

    // Resolve every requested executor up front, so one bad name fails the
    // whole dispatch instead of half-starting it.
    let mut resolved: Vec<(&str, &str)> = Vec::new();
    for name in &names {
        let Some(executor) = config.executors.iter().find(|e| &e.name == name) else {
            let available = config
                .executors
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return ToolResult::error(format!(
                "Error: unknown executor {name:?}. Available: {available}"
            ));
        };
        resolved.push((&executor.name, &executor.kind));
    }

    // The workspace gate applies once for the whole fan-out: every executor
    // works the same workspace, so one approval covers all of them.
    let workspace_path = super::helpers::resolve_tool_path(workspace, &ctx.project_root);
    let workspace_str = workspace_path.display().to_string();
    let is_within = sandbox::is_within_sandbox(&workspace_str, &ctx.workspace_root);
    let is_protected = sandbox::is_protected_path(&workspace_str, &ctx.project_root);
    let nervs_own_repo =
        ctx.project_root.join(".git").exists() && workspace_path.starts_with(&ctx.project_root);
    if !approved && (!is_within || is_protected || (nervs_own_repo && mode == "edit")) {
        // Queue ONE pending action covering the whole fan-out, like the
        // workflow executor's run approval — a confirmed run passes the gate.
        let store = PendingActionStore::new(&ctx.project_root);
        let reason = format!(
            "dispatch_agents will run {names:?} in '{workspace_str}' (mode {mode}), which \
             is outside the sandbox or a protected/own-repository location."
        );
        let record = match store.add(
            "dispatch_agents",
            args.clone(),
            &reason,
            "",
            "",
            &ctx.scope,
            "",
        ) {
            Ok(record) => record,
            Err(e) => {
                return ToolResult::error(format!("Failed to queue the dispatch: {e}"));
            }
        };
        return ToolResult::error(format!(
            "[ACTION SUSPENDED #{}] {reason}. This dispatch was NOT started. The user \
             must approve it in Nerv with 'confirm {}' before it runs.",
            record.id, record.id
        ));
    }

    // Queue all tasks. Each executor respects the config's budget/time caps.
    let mut ids: Vec<String> = Vec::new();
    for (name, kind) in &resolved {
        match enqueue_delegation(
            &ctx.project_root,
            name,
            kind,
            mode,
            task,
            &workspace_str,
            config.max_budget_usd,
            config.max_timeout_s,
        ) {
            Ok(id) => ids.push(id),
            Err(e) => return ToolResult::error(format!("Failed to create task for {name}: {e}")),
        }
    }

    ToolResult::ok(format!(
        "Dispatched {} executor(s) in parallel: {}. Each reports independently when \
         done; track them with 'delegation_status <task_id>' or cancel with \
         'cancel <task_id>'.",
        ids.len(),
        ids.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A configured executor list with one claude + one codex entry.
    fn executor_config(tmp: &Path) -> nerv_shared::config::NervConfig {
        // The loader always reads core/config.default.yaml first; stage it.
        std::fs::create_dir_all(tmp.join("core")).unwrap();
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("config.default.yaml"),
            tmp.join("core/config.default.yaml"),
        )
        .unwrap();
        let yaml = "delegation:\n  executors:\n    - name: claude\n      description: d\n      kind: claude\n    - name: codex\n      description: d\n      kind: codex\n  max_budget_usd: 2.0\n  max_timeout_s: 900\n";
        std::fs::write(tmp.join("config.yaml"), yaml).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                tmp.join("config.yaml"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        nerv_shared::config::NervConfig::load(tmp).unwrap()
    }

    #[test]
    fn enqueue_delegation_persists_a_queued_task() {
        let tmp = tempfile::tempdir().unwrap();
        let id = enqueue_delegation(
            tmp.path(),
            "claude",
            "claude",
            "read_only",
            "fix the bug",
            tmp.path().join("ws").to_str().unwrap(),
            2.0,
            900,
        )
        .unwrap();
        let store = nerv_shared::background_tasks::BackgroundTaskStore::new(tmp.path());
        let task = store.get(&id).expect("task persisted");
        let task = task.expect("task is present");
        assert_eq!(task.status, "queued");
        assert_eq!(task.payload["executor"], "claude");
    }

    #[tokio::test]
    async fn dispatch_agents_queues_one_task_per_executor() {
        let tmp = tempfile::tempdir().unwrap();
        executor_config(tmp.path());
        // The sandbox check resolves the workspace path, so it must exist.
        std::fs::create_dir_all(tmp.path().join("personal/workspace")).unwrap();
        // macOS: /var is a symlink to /private/var, and the sandbox check
        // canonicalizes the root — use the canonical root for both fields.
        let root = tmp.path().canonicalize().unwrap();
        let ctx = ToolContext {
            project_root: root.clone(),
            workspace_root: root.clone(),
            scope: "cli".to_string(),
            confirmed: None,
        };
        let args = json!({
            "executors": ["claude", "codex"],
            "task": "review the module",
            "workspace": "personal/workspace",
            "mode": "read_only",
        });
        // The workspace is protected (personal/**), so the first call queues
        // a whole-fan-out approval; confirming it runs the dispatch.
        let first = dispatch_agents_impl(&ctx, &args).await;
        assert!(first.is_error, "{}", first.content);
        assert!(
            first.content.contains("ACTION SUSPENDED"),
            "{}",
            first.content
        );

        let pending = PendingActionStore::new(&ctx.project_root).list();
        assert_eq!(pending.len(), 1, "one approval, not one per executor");
        let record = pending[0].clone();
        PendingActionStore::new(&ctx.project_root).remove(record.id);

        let ctx = ToolContext {
            project_root: ctx.project_root.clone(),
            workspace_root: ctx.workspace_root.clone(),
            scope: ctx.scope.clone(),
            confirmed: Some(record),
        };
        let result = dispatch_agents_impl(&ctx, &args).await;
        assert!(!result.is_error, "{}", result.content);

        let store = nerv_shared::background_tasks::BackgroundTaskStore::new(tmp.path());
        let tasks = store.list_by_status("queued");
        assert_eq!(tasks.len(), 2, "one per executor");
        assert!(result.content.contains(&tasks[0].id));
        assert!(result.content.contains(&tasks[1].id));
    }

    #[tokio::test]
    async fn dispatch_agents_rejects_unknown_executors_up_front() {
        let tmp = tempfile::tempdir().unwrap();
        executor_config(tmp.path());
        // macOS: /var is a symlink to /private/var, and the sandbox check
        // canonicalizes the root — use the canonical root for both fields.
        let root = tmp.path().canonicalize().unwrap();
        let ctx = ToolContext {
            project_root: root.clone(),
            workspace_root: root.clone(),
            scope: "cli".to_string(),
            confirmed: None,
        };
        let args = json!({
            "executors": ["claude", "ghost"],
            "task": "t",
            "workspace": "personal/workspace",
        });
        let result = dispatch_agents_impl(&ctx, &args).await;
        assert!(result.is_error);
        // Half-started dispatches are worse than none: nothing was queued.
        let store = nerv_shared::background_tasks::BackgroundTaskStore::new(tmp.path());
        assert!(store.list_by_status("queued").is_empty());
    }
}
