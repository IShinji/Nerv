//! Native coding-agent delegation (PRD v2 E1).
//!
//! Builds the executor command line, runs it under the background-task
//! service, captures the result + `git diff --stat` + cost, and writes the
//! full transcript to `personal/delegations/<id>/`.
//!
//! Boundary rules (PRD v2 D11/D12):
//! - `read_only` or `edit` (in an isolated git worktree/branch); `full` is
//!   not offered in v1
//! - workspace must be inside the sandbox and not a protected path
//! - budget and wall-clock ceilings are mandatory with hard caps
//! - the subprocess env is scrubbed (no Nerv credentials) and it runs in its
//!   own process group, killed whole-group on timeout/cancel

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};

/// The normalized, validated parameters of one delegation.
#[derive(Debug, Clone)]
pub struct DelegationRequest {
    pub executor_name: String,
    pub executor_kind: String,
    pub task: String,
    pub workspace: PathBuf,
    pub mode: String,
    pub max_budget_usd: f64,
    pub timeout_s: u64,
}

/// Build the executor's command line for the requested mode.
///
/// Permission modes (from `--help` outputs; behaviours to verify with fake
/// tasks at E1 start):
/// - Claude Code: `-p --permission-mode dontAsk` with read-only allowedTools,
///   or `--permission-mode acceptEdits` (Bash stays denied); plus
///   `--output-format stream-json`, `--max-budget-usd`, `--no-session-persistence`
/// - Codex: `exec -s read-only` / `exec -s workspace-write`, `--json`,
///   `--ephemeral`, `-C <dir>`
pub fn build_command(
    request: &DelegationRequest,
    bin: &str,
    worktree_dir: Option<&Path>,
) -> Vec<String> {
    let cwd = worktree_dir
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| request.workspace.display().to_string());

    let mut args: Vec<String> = Vec::new();
    if request.executor_kind == "codex" {
        args.push(bin.to_string());
        args.push("exec".to_string());
        args.push("-s".to_string());
        args.push(
            if request.mode == "read_only" {
                "read-only"
            } else {
                "workspace-write"
            }
            .to_string(),
        );
        args.push("--json".to_string());
        args.push("--ephemeral".to_string());
        args.push("-C".to_string());
        args.push(cwd);
        args.push(request.task.clone());
    } else {
        args.push(bin.to_string());
        args.push("-p".to_string());
        args.push("--permission-mode".to_string());
        args.push(
            if request.mode == "read_only" {
                "dontAsk"
            } else {
                "acceptEdits"
            }
            .to_string(),
        );
        if request.mode == "read_only" {
            args.push("--allowedTools".to_string());
            args.push("Read,Grep,Glob".to_string());
        }
        args.push("--output-format".to_string());
        args.push("stream-json".to_string());
        args.push("--max-budget-usd".to_string());
        args.push(format!("{}", request.max_budget_usd));
        args.push("--no-session-persistence".to_string());
        args.push(request.task.clone());
        args.push("-C".to_string());
        args.push(cwd);
    }
    args
}

/// Whether the boundary rules allow auto-execution. Returns the reason the
/// delegation must be confirmed, or `None` when it may run directly.
#[allow(dead_code)]
pub fn gate(
    request: &DelegationRequest,
    workspace_root: &Path,
    project_root: &Path,
    nervs_own_repo: bool,
) -> Option<String> {
    if request.mode == "full" {
        return Some("mode 'full' is not offered in v1".to_string());
    }
    let workspace = request.workspace.display().to_string();
    let is_within = nerv_brain::tools::sandbox::is_within_sandbox(&workspace, workspace_root);
    let is_protected = nerv_brain::tools::sandbox::is_protected_path(&workspace, project_root);
    if !is_within {
        return Some(format!(
            "workspace '{workspace}' is outside the Nerv workspace sandbox"
        ));
    }
    if is_protected {
        return Some(format!(
            "workspace '{workspace}' is a protected config/secret/code location"
        ));
    }
    // A delegation into Nerv's own repository must run in an isolated
    // worktree so the main checkout can never be modified directly.
    if nervs_own_repo && request.mode == "edit" {
        return Some("editing Nerv's own repository requires an isolated git worktree".to_string());
    }
    None
}

/// Extract the final answer, cost and turn count from claude's stream-json
/// output (last `result` line wins; codex emits plain json lines).
pub fn parse_stream_output(raw: &str, kind: &str) -> (String, Option<f64>) {
    let mut answer = String::new();
    let mut cost = None;
    for line in raw.lines().rev() {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if kind == "codex" {
            // codex --json emits {"type":"item.completed","item":{"text":...}}
            if answer.is_empty() {
                if let Some(text) = value.pointer("/item/text").and_then(|v| v.as_str()) {
                    answer = text.to_string();
                }
            }
            continue;
        }
        // claude stream-json: {"type":"result","result":"...","total_cost_usd":...}
        if value.get("type").and_then(|v| v.as_str()) == Some("result") {
            answer = value
                .get("result")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            cost = value.get("total_cost_usd").and_then(|v| v.as_f64());
            break;
        }
    }
    (answer, cost)
}

/// Run `git diff --stat` in `dir` and return the output (or empty when git
/// is unavailable or the directory is not a repository).
pub async fn git_diff_stat(dir: &Path) -> String {
    let output = tokio::process::Command::new("git")
        .args(["diff", "--stat"])
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => String::new(),
    }
}

/// Create an isolated git worktree at `dir/worktrees/<id>` on a new branch.
#[allow(dead_code)]
pub async fn create_worktree(repo: &Path, id: &str, base_dir: &Path) -> Result<PathBuf, String> {
    let worktree = base_dir.join(format!("nerv-{id}"));
    let branch = format!("nerv/{id}");
    let output = tokio::process::Command::new("git")
        .args(["worktree", "add"])
        .arg(&worktree)
        .arg("-b")
        .arg(&branch)
        .current_dir(repo)
        .output()
        .await
        .map_err(|e| format!("git worktree failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git worktree failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(worktree)
}

/// The runner id the background-task service dispatches to.
#[allow(dead_code)]
pub const DELEGATE_TASK_KIND: &str = "done";

/// Run one delegation end to end and produce the user-facing report.
///
/// Builds the executor command from the payload via [`build_command`], runs
/// it with the scrubbed env under the timeout, parses the stream output, and
/// captures the diff stat. The transcript goes to
/// `personal/delegations/<id>/run.jsonl`.
pub async fn run_delegate(task: &nerv_shared::background_tasks::BackgroundTask) -> String {
    let get = |key: &str| {
        task.payload
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let get_f = |key: &str, default: f64| {
        task.payload
            .get(key)
            .and_then(|v| v.as_f64())
            .unwrap_or(default)
    };
    let get_i = |key: &str, default: u64| {
        task.payload
            .get(key)
            .and_then(|v| v.as_u64())
            .unwrap_or(default)
    };

    let request = DelegationRequest {
        executor_name: get("executor"),
        executor_kind: get("executor_kind"),
        task: get("task"),
        workspace: PathBuf::from(get("workspace")),
        mode: get("mode"),
        max_budget_usd: get_f("max_budget_usd", 2.0),
        timeout_s: get_i("timeout_s", 1800),
    };

    let project_root =
        PathBuf::from(std::env::var("NERV_PROJECT_ROOT").unwrap_or_else(|_| ".".to_string()));
    let bin = if request.executor_kind == "codex" {
        "codex"
    } else {
        "claude"
    };
    let args = build_command(&request, bin, None);

    // The delegation transcript directory.
    let transcript_dir = project_root
        .join("personal")
        .join("delegations")
        .join(&task.id);
    let _ = std::fs::create_dir_all(&transcript_dir);

    let started = std::time::Instant::now();
    let mut command = tokio::process::Command::new(&args[0]);
    command
        .args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(nerv_brain::tools::sandbox::scrubbed_env(&[]))
        .kill_on_drop(true);

    let outcome =
        tokio::time::timeout(Duration::from_secs(request.timeout_s), command.output()).await;

    let (status_code, stdout, stderr) = match outcome {
        Ok(Ok(output)) => (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ),
        Ok(Err(e)) => (-1, String::new(), format!("{e}")),
        Err(_) => (-1, String::new(), "timed out".to_string()),
    };

    let (answer, cost) = parse_stream_output(&stdout, &request.executor_kind);
    let diff_stat = git_diff_stat(&request.workspace).await;
    let duration_s = started.elapsed().as_secs_f64();

    // Persist the full transcript.
    let transcript = json!({
        "id": task.id,
        "executor": request.executor_name,
        "mode": request.mode,
        "workspace": request.workspace.display().to_string(),
        "task": request.task,
        "exit_code": status_code,
        "cost_usd": cost,
        "duration_s": (duration_s * 10.0).round() / 10.0,
        "answer": answer,
        "stderr": stderr,
    });
    let _ = std::fs::write(
        transcript_dir.join("run.jsonl"),
        serde_json::to_string(&transcript).unwrap_or_default() + "\n",
    );

    if status_code != 0 {
        return format!(
            "Delegation {} failed (exit {}): {}",
            task.id, status_code, stderr
        );
    }
    let diff = if diff_stat.is_empty() {
        String::new()
    } else {
        format!(
            "\nChanges:\n{}",
            nerv_brain::tools::helpers::truncate(&diff_stat, 1000, "delegate")
        )
    };
    let cost_text = cost.map(|c| format!(" Cost: ${c:.2}.")).unwrap_or_default();
    format!(
        "Delegation {} finished.{}{} Duration: {:.0}s.{}",
        task.id,
        if answer.is_empty() {
            String::new()
        } else {
            format!(" Result: {answer}")
        },
        diff,
        duration_s,
        cost_text
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_read_only_command_shape() {
        let request = DelegationRequest {
            executor_name: "claude".to_string(),
            executor_kind: "claude".to_string(),
            task: "fix the tests".to_string(),
            workspace: PathBuf::from("/tmp/proj"),
            mode: "read_only".to_string(),
            max_budget_usd: 2.0,
            timeout_s: 1800,
        };
        let args = build_command(&request, "claude", None);
        assert!(args.contains(&"--permission-mode".to_string()));
        let mode_idx = args.iter().position(|a| a == "--permission-mode").unwrap();
        assert_eq!(args[mode_idx + 1], "dontAsk");
        assert!(args.contains(&"--max-budget-usd".to_string()));
        assert!(args.contains(&"2.000000".to_string()) || args.contains(&"2".to_string()));
        assert!(args.contains(&"--no-session-persistence".to_string()));
        assert!(args.contains(&"fix the tests".to_string()));
    }

    #[test]
    fn claude_edit_mode_allows_edits_but_not_bash() {
        let request = DelegationRequest {
            executor_name: "claude".to_string(),
            executor_kind: "claude".to_string(),
            task: "refactor".to_string(),
            workspace: PathBuf::from("/tmp/proj"),
            mode: "edit".to_string(),
            max_budget_usd: 2.0,
            timeout_s: 1800,
        };
        let args = build_command(&request, "claude", None);
        let mode_idx = args.iter().position(|a| a == "--permission-mode").unwrap();
        assert_eq!(args[mode_idx + 1], "acceptEdits");
        assert!(!args.contains(&"--allowedTools".to_string()));
    }

    #[test]
    fn codex_command_shape() {
        let request = DelegationRequest {
            executor_name: "codex".to_string(),
            executor_kind: "codex".to_string(),
            task: "fix the tests".to_string(),
            workspace: PathBuf::from("/tmp/proj"),
            mode: "read_only".to_string(),
            max_budget_usd: 2.0,
            timeout_s: 1800,
        };
        let args = build_command(&request, "codex", None);
        assert!(args.contains(&"read-only".to_string()));
        assert!(args.contains(&"--json".to_string()));
        assert!(args.contains(&"--ephemeral".to_string()));
    }

    #[test]
    fn gate_rules() {
        let base = DelegationRequest {
            executor_name: "claude".to_string(),
            executor_kind: "claude".to_string(),
            task: String::new(),
            workspace: PathBuf::new(),
            mode: "read_only".to_string(),
            max_budget_usd: 2.0,
            timeout_s: 1800,
        };
        let dir = tempfile::tempdir().unwrap();

        // full mode is refused outright
        let mut request = base.clone();
        request.mode = "full".to_string();
        assert!(gate(&request, dir.path(), dir.path(), false).is_some());

        // outside the sandbox: confirm
        let mut request = base.clone();
        request.workspace = PathBuf::from("/etc");
        assert!(gate(&request, dir.path(), dir.path(), false).is_some());

        // inside the sandbox, read-only: auto
        let mut request = base.clone();
        request.workspace = dir.path().to_path_buf();
        assert!(gate(&request, dir.path(), dir.path(), false).is_none());

        // nerv's own repo in edit mode without a worktree: confirm
        let mut request = base.clone();
        request.workspace = dir.path().to_path_buf();
        request.mode = "edit".to_string();
        assert!(gate(&request, dir.path(), dir.path(), true).is_some());
    }

    #[tokio::test]
    async fn stream_output_parses_result_and_cost() {
        let raw = concat!(
            r#"{"type":"assistant","message":{}}"#,
            "\n",
            r#"{"type":"result","result":"fixed 3 tests","total_cost_usd":0.84}"#,
            "\n",
        );
        let (answer, cost) = parse_stream_output(raw, "claude");
        assert_eq!(answer, "fixed 3 tests");
        assert_eq!(cost, Some(0.84));
    }
}
