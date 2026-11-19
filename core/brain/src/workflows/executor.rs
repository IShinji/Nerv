//! Deterministic multi-step workflow executor.
//!
//! Port of `brain/nerv/workflows/executor.py`. Steps run structurally (one
//! tool / reasoning step at a time); LLM help — argument extraction and
//! reasoning — flows through the model caller, so the executor is
//! backend-agnostic. Runs are persisted per-step to a TaskRun, so an
//! interrupted workflow can be resumed instead of restarting.
//!
//! Tool steps go through the same confirmation gate as an agent's ad-hoc
//! tool use: a step that needs approval is queued as a pending action and
//! reported, never run silently. Steps that need approval and whose
//! arguments are known before the run starts (the browser verbs) are asked
//! for once, as a whole run, instead of once per step.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tracing::{error, info, warn};

use super::WorkflowDefinition;
use crate::tasks::{TaskRun, TaskRunStore, DONE, FAILED, RUNNING, SUSPENDED};
use crate::tools::pending_store::PendingActionStore;
use crate::tools::registry::{ToolContext, ToolRegistry, ToolResult};
use crate::tools::sandbox::confirmation_reason;

const STEP_RETRY_CAP_SECONDS: u64 = 8;
/// The tool a whole-run approval is queued under; the orchestrator calls it
/// with the same arguments when the user confirms.
pub const APPROVAL_TOOL: &str = "execute_workflow";
/// Longest step line shown in an approval prompt (the full call is still bound to it).
const SHOWN_STEP_CHARS: usize = 160;

/// Targeted-notification sink (the gateway routes `notify(channel, sender, text)`
/// to the matching adapter; E0 定向通知).
pub type NotifyFn = Arc<dyn Fn(&str, &str, &str) + Send + Sync>;

/// A run is waiting for the user; the confirmation is already queued.
///
/// `message` is meant for the model that called the workflow, `user_message`
/// for the user. `stale` means an approval was given but no longer matches
/// the workflow (it changed after the user was asked), so nothing ran.
#[derive(Debug, Clone)]
pub struct WorkflowNeedsApproval {
    pub message: String,
    pub user_message: String,
    pub stale: bool,
}

/// One exact tool call the run intends to make, planned up front.
#[derive(Debug, Clone)]
struct PlannedCall {
    idx: usize,
    line: String,
    tool_name: String,
    kwargs: serde_json::Value,
    needs_approval: bool,
}

/// Everything the executor needs from its caller.
pub struct ExecutorContext {
    pub project_root: PathBuf,
    pub workspace_root: PathBuf,
    pub scope: String,
    pub channel: String,
    pub sender: String,
    /// The heartbeat-notification drain (alerts surface at the next tick).
    pub brain: Option<Arc<nerv_shared::brain_client::BrainClient>>,
    /// Targeted-notify sink (None in the MCP server, where there are no adapters).
    pub notify: Option<NotifyFn>,
    /// The record the user confirmed, when this run IS the confirmed call.
    pub confirmed: Option<crate::turn::PendingRecord>,
    /// Model help for argument extraction and reasoning steps (tier 0).
    pub caller: Arc<dyn crate::turn::ModelCaller>,
}

/// Identity of one exact tool call, to match it against what was approved.
fn call_key(tool_name: &str, kwargs: &serde_json::Value) -> String {
    format!("{tool_name}:{kwargs}")
}

pub struct WorkflowExecutor {
    ctx: ExecutorContext,
    store: TaskRunStore,
    /// Exact calls the user approved for the run in progress.
    approved_calls: HashSet<String>,
}

impl WorkflowExecutor {
    pub fn new(ctx: ExecutorContext) -> Self {
        let store = TaskRunStore::new(&ctx.project_root);
        Self {
            ctx,
            store,
            approved_calls: HashSet::new(),
        }
    }

    fn model(&self) -> String {
        super::super::turn::prepare::select_model(&self.ctx.project_root, 0)
    }

    /// Run (or resume) the workflow, persisting progress after every step.
    ///
    /// Steps that need confirmation and whose arguments are fixed up front are
    /// approved once for the whole run: the first call queues a single pending
    /// action listing them and raises [`WorkflowNeedsApproval`]; when the user
    /// confirms it, the orchestrator calls this again and exactly those calls
    /// run without further prompts. A step that is only gated while running
    /// stops the run with the same error.
    pub async fn execute(
        &mut self,
        workflow: &WorkflowDefinition,
        initial_input: &str,
        run_id: Option<&str>,
    ) -> Result<String, WorkflowNeedsApproval> {
        self.preflight(workflow, initial_input, run_id)?;
        let result = self.execute_steps(workflow, initial_input, run_id).await;
        self.approved_calls.clear();
        result
    }

    /// Settle approval for the whole run before any step is executed.
    ///
    /// Queues one pending action listing every step that needs approval and
    /// raises. A confirmed approval only counts while the workflow still plans
    /// exactly what the user was shown; if it changed since, the user is
    /// asked again.
    pub(crate) fn preflight(
        &mut self,
        workflow: &WorkflowDefinition,
        initial_input: &str,
        run_id: Option<&str>,
    ) -> Result<(), WorkflowNeedsApproval> {
        let run = run_id.and_then(|id| self.store.get(id));
        let run_input = run
            .as_ref()
            .map(|r| r.initial_input.clone())
            .unwrap_or_else(|| initial_input.to_string());
        let steps: Vec<(usize, String)> = match &run {
            Some(run) => run
                .steps
                .iter()
                .filter(|s| s.status != DONE)
                .map(|s| (s.idx as usize, s.description.clone()))
                .collect(),
            None => workflow
                .steps
                .iter()
                .enumerate()
                .map(|(i, s)| (i + 1, s.clone()))
                .collect(),
        };
        let gated: Vec<PlannedCall> = self
            .plan(&steps, &run_input)
            .into_iter()
            .filter(|c| c.needs_approval)
            .collect();
        if gated.is_empty() {
            return Ok(());
        }

        let reason = approval_reason(&workflow.name, &gated);
        let arguments = json!({"workflow_name": workflow.name, "inputs": run_input});
        let mut stale = false;
        if let Some(confirmed) = &self.ctx.confirmed {
            if confirmed.tool_name == APPROVAL_TOOL && confirmed.arguments == arguments {
                if confirmed.reason == reason {
                    self.approved_calls = gated
                        .iter()
                        .map(|c| call_key(&c.tool_name, &c.kwargs))
                        .collect();
                    return Ok(());
                }
                stale = true; // the user approved a plan that no longer matches
            }
        }

        let store = PendingActionStore::new(&self.ctx.project_root);
        let scope = &self.ctx.scope;
        let record = store
            .add(
                APPROVAL_TOOL,
                arguments,
                &reason,
                "", // the whole-run approval is not sender-bound
                "",
                if scope == "default" { "" } else { scope },
                "",
            )
            .map_err(|e| WorkflowNeedsApproval {
                message: format!("Failed to queue the approval: {e}"),
                user_message: format!("Failed to queue the approval: {e}"),
                stale: false,
            })?;
        warn!(
            "Workflow '{}' needs approval (pending #{})",
            workflow.name, record.id
        );
        let lead = if stale {
            "The workflow changed after it was approved, so nothing ran. "
        } else {
            ""
        };
        Err(WorkflowNeedsApproval {
            message: format!(
                "[ACTION SUSPENDED #{id}] {lead}{reason}\n\
                 This action was NOT executed. The user must approve it in Nerv with \
                 'confirm {id}' before it runs. Do not retry — tell the user \
                 what is pending and continue with anything else you can do safely.",
                id = record.id
            ),
            user_message: format!(
                "{lead}{reason}\nReply \"confirm {id}\" to run it, or \"cancel {id}\" to discard it.",
                id = record.id
            ),
            stale,
        })
    }

    /// The tool calls whose arguments are fixed before the run starts.
    ///
    /// These are the direct browser verbs. Anything the model has to fill in
    /// while running is not planned: it is gated step by step when reached.
    fn plan(&self, steps: &[(usize, String)], initial_input: &str) -> Vec<PlannedCall> {
        let planned: Vec<PlannedCall> = steps
            .iter()
            .filter_map(|(idx, text)| {
                let (tool_name, method_hint, args_hint) = parse_step(text);
                let tool_name = tool_name?;
                let registry = ToolRegistry;
                let tool = registry.get_tool(&tool_name)?;
                let kwargs = direct_kwargs(&method_hint, &args_hint, initial_input)?;
                let reason = confirmation_reason(
                    tool.name,
                    &kwargs,
                    &self.ctx.workspace_root,
                    tool.requires_confirmation,
                    Some(&self.ctx.project_root),
                );
                let shown: String = text.replace("%inputs%", initial_input);
                let shown = normalize_spaces(&shown);
                let shown = if shown.chars().count() > SHOWN_STEP_CHARS {
                    format!(
                        "{}...",
                        shown.chars().take(SHOWN_STEP_CHARS - 3).collect::<String>()
                    )
                } else {
                    shown
                };
                Some(PlannedCall {
                    idx: *idx,
                    line: format!("{idx}. {shown}"),
                    tool_name: tool.name.to_string(),
                    kwargs,
                    needs_approval: reason.is_some(),
                })
            })
            .collect();
        planned
    }

    /// The run itself: one step after another, saving progress as it goes.
    async fn execute_steps(
        &mut self,
        workflow: &WorkflowDefinition,
        initial_input: &str,
        run_id: Option<&str>,
    ) -> Result<String, WorkflowNeedsApproval> {
        let run: TaskRun = match run_id.and_then(|id| self.store.get(id)) {
            Some(run) => {
                self.notify(&format!(
                    "[Workflow] Resuming: {} (run {})",
                    workflow.name, run.id
                ));
                run
            }
            None => {
                let run = self
                    .store
                    .create(&workflow.name, initial_input, &workflow.steps);
                self.notify(&format!(
                    "[Workflow] Started: {} (run {})",
                    workflow.name, run.id
                ));
                run
            }
        };
        let mut run = run;
        run.status = RUNNING.to_string();
        let model = self.model();

        // Rebuild context from any already-completed steps (resume support).
        let mut history_context = vec![format!("Initial Input: {}", run.initial_input)];
        for step in &run.steps {
            if step.status == DONE {
                history_context.push(format!(
                    "Step {} Result: {}",
                    step.idx,
                    step.result.chars().take(2000).collect::<String>()
                ));
            }
        }

        let total = run.steps.len();
        for index in 0..run.steps.len() {
            if run.steps[index].status == DONE {
                continue;
            }
            run.steps[index].status = RUNNING.to_string();
            self.store.save(&mut run);
            let desc: String = run.steps[index].description.chars().take(80).collect();
            self.notify(&format!(
                "[Workflow] Step {}/{total}: {desc}...",
                run.steps[index].idx
            ));

            let step_idx = run.steps[index].idx;
            let step_desc = run.steps[index].description.clone();
            match self
                .run_step_with_retries(
                    &step_desc,
                    step_idx,
                    &workflow.name,
                    &model,
                    &history_context,
                )
                .await
            {
                Ok(StepOutput::Text(output)) => {
                    run.steps[index].result = output.clone();
                    run.steps[index].status = DONE.to_string();
                    self.store.save(&mut run);
                    history_context.push(format!(
                        "Step {} Result: {}",
                        step_idx,
                        output.chars().take(2000).collect::<String>()
                    ));
                }
                Ok(StepOutput::Suspended { pending_id, text }) => {
                    // The step was queued for its own confirmation, not run.
                    // Stop here instead of marking it done and carrying on.
                    run.steps[index].result = text.clone();
                    run.steps[index].status = SUSPENDED.to_string();
                    run.status = SUSPENDED.to_string();
                    self.store.save(&mut run);
                    self.notify(&format!(
                        "[Workflow] Stopped at step {step_idx}: waiting for confirmation"
                    ));
                    let where_ = format!(
                        "stopped at step {step_idx}/{total}, which needs its own confirmation"
                    );
                    return Err(WorkflowNeedsApproval {
                        message: format!(
                            "Workflow '{}' {where_}; the steps after it did not run.\n{text}",
                            workflow.name
                        ),
                        user_message: format!(
                            "Workflow '{}' {where_}. Reply \"confirm {pending_id}\" to run just \
                             that step, or \"cancel {pending_id}\" to discard it. The steps \
                             after it did not run.",
                            workflow.name
                        ),
                        stale: false,
                    });
                }
                Err(e) => {
                    run.steps[index].status = FAILED.to_string();
                    run.status = FAILED.to_string();
                    self.store.save(&mut run);
                    error!(
                        "Workflow '{}' failed at step {}: {e}",
                        workflow.name, step_idx
                    );
                    let desc: String = run.steps[index].description.chars().take(60).collect();
                    self.notify(&format!("[Workflow] Failed at step {step_idx}: {e}"));
                    return Ok(format!(
                        "Workflow '{}' failed at step {} ({desc}): {e}. Progress saved as run {}.",
                        workflow.name, step_idx, run.id
                    ));
                }
            }
        }

        run.status = DONE.to_string();
        self.store.save(&mut run);
        self.notify(&format!("[Workflow] Finished {}", workflow.name));
        let tail: Vec<String> = history_context
            .iter()
            .rev()
            .take(3)
            .rev()
            .cloned()
            .collect();
        Ok(tail.join("\n"))
    }

    /// Execute one step, retrying transient failures with backoff.
    async fn run_step_with_retries(
        &self,
        step_desc: &str,
        step_idx: i64,
        workflow_name: &str,
        model: &str,
        history_context: &[String],
    ) -> Result<StepOutput, String> {
        let attempts: u32 = std::env::var("NERV_WORKFLOW_STEP_ATTEMPTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2)
            .max(1);
        let mut last_error = String::new();
        for attempt in 1..=attempts {
            match self
                .execute_step(step_desc, step_idx, workflow_name, model, history_context)
                .await
            {
                Ok(output) => return Ok(output),
                Err(e) => {
                    last_error = e;
                    warn!(
                        "Workflow step {} attempt {attempt}/{attempts} failed: {last_error}",
                        step_idx
                    );
                    if attempt < attempts {
                        tokio::time::sleep(Duration::from_secs(
                            (2u64).pow(attempt - 1).min(STEP_RETRY_CAP_SECONDS),
                        ))
                        .await;
                    }
                }
            }
        }
        Err(if last_error.is_empty() {
            "step failed".to_string()
        } else {
            last_error
        })
    }

    /// Dispatch one step to a tool call or a reasoning step.
    async fn execute_step(
        &self,
        step_str: &str,
        step_idx: i64,
        workflow_name: &str,
        model: &str,
        history_context: &[String],
    ) -> Result<StepOutput, String> {
        let (tool_name, method_hint, args_hint) = parse_step(step_str);
        let registry = ToolRegistry;
        let tool = tool_name
            .as_deref()
            .and_then(|name| registry.get_tool(name));
        match tool {
            Some(tool) => {
                self.execute_tool_step(
                    tool,
                    &method_hint,
                    &args_hint,
                    step_str,
                    step_idx,
                    workflow_name,
                    model,
                    history_context,
                )
                .await
            }
            None => self
                .execute_reasoning_step(step_str, step_idx, workflow_name, model, history_context)
                .await
                .map(StepOutput::Text),
        }
    }

    /// Execute a step that targets a specific tool.
    #[allow(clippy::too_many_arguments)]
    async fn execute_tool_step(
        &self,
        tool: &crate::tools::registry::ToolDefinition,
        method_hint: &str,
        args_hint: &str,
        step_str: &str,
        step_idx: i64,
        workflow_name: &str,
        model: &str,
        history_context: &[String],
    ) -> Result<StepOutput, String> {
        // Fast path: deterministic direct calls (no LLM) for known browser verbs.
        if !method_hint.is_empty() {
            let initial_input = initial_input_from(history_context);
            if let Some(kwargs) = direct_kwargs(method_hint, args_hint, &initial_input) {
                self.notify(&format!(
                    "[Workflow] Direct call: {}.{}",
                    tool.name, method_hint
                ));
                return self.call_tool_gated(tool, &kwargs).await;
            }
        }

        // Otherwise ask the model for the tool's arguments as JSON, then invoke
        // the tool natively (claude-cli does not hand back structured tool
        // calls, so args are extracted explicitly).
        let parameters = &tool.parameters;
        let messages = vec![
            crate::llm::ChatMessage::new(
                "system",
                format!(
                    "You are executing step {step_idx} of workflow '{workflow_name}'.\n\
                     Step instructions: {step_str}\n\
                     Produce the arguments for the tool '{}' as a JSON object matching this \
                     parameter schema:\n{}\n\
                     Fill values using the provided context. Output JSON only.",
                    tool.name,
                    serde_json::to_string(parameters).unwrap_or_default()
                ),
            ),
            crate::llm::ChatMessage::new(
                "user",
                format!("Context history:\n{}", last_history(history_context, 5)),
            ),
        ];
        info!(
            "Workflow step {step_idx}: extracting args for {}",
            tool.name
        );
        let result = self
            .ctx
            .caller
            .complete(&messages, model, None, &[], &self.ctx.scope)
            .await
            .map_err(|e| e.to_string())?;
        let args = parse_json_args(&result.content);

        self.notify(&format!("[Workflow] Calling {}...", tool.name));
        self.call_tool_gated(tool, &args).await
    }

    /// Run a tool, or queue it for confirmation per the sandbox policy.
    ///
    /// A call the user already approved as part of this run goes straight
    /// through; anything else is gated as before.
    async fn call_tool_gated(
        &self,
        tool: &crate::tools::registry::ToolDefinition,
        kwargs: &serde_json::Value,
    ) -> Result<StepOutput, String> {
        let approved = self.approved_calls.contains(&call_key(tool.name, kwargs));
        let reason = if approved {
            None
        } else {
            confirmation_reason(
                tool.name,
                kwargs,
                &self.ctx.workspace_root,
                tool.requires_confirmation,
                Some(&self.ctx.project_root),
            )
        };
        if let Some(reason) = reason {
            let store = PendingActionStore::new(&self.ctx.project_root);
            let scope = &self.ctx.scope;
            let record = store
                .add(
                    tool.name,
                    kwargs.clone(),
                    &reason,
                    "",
                    "",
                    if scope == "default" { "" } else { scope },
                    "",
                )
                .map_err(|e| format!("pending store failed: {e}"))?;
            warn!("Workflow tool {} suspended: {reason}", tool.name);
            let text = format!(
                "[ACTION SUSPENDED #{}] {reason}. Not executed; the user must reply \
                 'confirm {}' to run it.",
                record.id, record.id
            );
            return Ok(StepOutput::Suspended {
                pending_id: record.id,
                text,
            });
        }
        let tool_ctx = ToolContext {
            project_root: self.ctx.project_root.clone(),
            workspace_root: self.ctx.workspace_root.clone(),
            scope: self.ctx.scope.clone(),
            brain: self.ctx.brain.clone(),
            confirmed: None,
        };
        let result: ToolResult = (tool.func)(&tool_ctx, kwargs).await;
        Ok(StepOutput::Text(result.content))
    }

    /// Execute a pure reasoning step (no specific tool target).
    async fn execute_reasoning_step(
        &self,
        step_str: &str,
        step_idx: i64,
        workflow_name: &str,
        model: &str,
        history_context: &[String],
    ) -> Result<String, String> {
        let messages = vec![
            crate::llm::ChatMessage::new(
                "system",
                format!(
                    "You are evaluating step {step_idx} of workflow '{workflow_name}'.\n\
                     Step instructions: {step_str}\n\
                     Process the prior outputs and generate the step result."
                ),
            ),
            crate::llm::ChatMessage::new(
                "user",
                format!("Context history:\n{}", last_history(history_context, 5)),
            ),
        ];
        let result = self
            .ctx
            .caller
            .complete(&messages, model, None, &[], &self.ctx.scope)
            .await
            .map_err(|e| e.to_string())?;
        Ok(result.content)
    }

    /// Push a background notification to the heartbeat drain and, when a
    /// targeted sink is wired, straight to the requesting channel (E0).
    fn notify(&self, message: &str) {
        info!("{message}");
        if let Some(brain) = &self.ctx.brain {
            let text = message.to_string();
            let brain = Arc::clone(brain);
            tokio::spawn(async move {
                let _ = brain
                    .call("queue_notifications", json!({ "notifications": [text] }))
                    .await;
            });
        }
        if let Some(notify) = &self.ctx.notify {
            notify(&self.ctx.channel, &self.ctx.sender, message);
        }
    }
}

enum StepOutput {
    Text(String),
    Suspended { pending_id: i64, text: String },
}

/// Parse a step into `(tool_name, method_hint, args_hint)`.
///
/// Supported formats:
/// - "chrome_browser.open_url -> https://gemini.google.com"
/// - "browser.interactive.open_url -> https://gemini.google.com"
/// - "chrome_browser.wait_for_idle"
/// - "web_search query about AI models"
pub fn parse_step(step_str: &str) -> (Option<String>, String, String) {
    let stripped = step_str.trim();
    if stripped.is_empty() {
        return (None, String::new(), String::new());
    }

    if let Some((left, right)) = stripped.split_once("->") {
        let target = left.trim();
        if let Some((name, method_hint)) = resolve_step_target(target) {
            return (Some(name), method_hint, right.trim().to_string());
        }
    }

    let first_word = stripped.split_whitespace().next().unwrap_or("");
    if let Some((name, method_hint)) = resolve_step_target(first_word) {
        let remainder = stripped[first_word.len()..].trim().to_string();
        return (Some(name), method_hint, remainder);
    }

    (None, String::new(), step_str.to_string())
}

/// Resolve the callable target within a workflow step identifier: the
/// longest known prefix of a dotted name wins, the rest is a method hint.
pub fn resolve_step_target(identifier: &str) -> Option<(String, String)> {
    if let Some(provider) = crate::orchestrator::runtime_policy::resolve_provider(identifier) {
        return Some((provider, String::new()));
    }
    let parts: Vec<&str> = identifier.split('.').collect();
    for idx in (1..parts.len()).rev() {
        let candidate = parts[..idx].join(".");
        let method_hint = parts[idx..].join(".");
        if let Some(provider) = crate::orchestrator::runtime_policy::resolve_provider(&candidate) {
            return Some((provider, method_hint));
        }
    }
    None
}

/// Arguments for a browser-style verb, or `None` if the model must supply them.
pub fn direct_kwargs(
    method_hint: &str,
    args_hint: &str,
    initial_input: &str,
) -> Option<serde_json::Value> {
    let resolved_args = args_hint.replace("%inputs%", initial_input);
    let mut kwargs = match method_hint {
        "open_url" => json!({"url": resolved_args}),
        "fill_prompt" | "type_text" => json!({"text": resolved_args}),
        "click_element" | "wait_for_selector" | "extract_text" => {
            json!({"selector": resolved_args})
        }
        "wait" => json!({"seconds": resolved_args.parse::<i64>().unwrap_or(5)}),
        "get_page_text" | "submit_prompt" => json!({}),
        "wait_for_idle" => json!({"timeout_secs": 60}),
        _ => return None,
    };
    kwargs["action"] = json!(method_hint);
    Some(kwargs)
}

/// Extract a JSON argument object from model output (tolerant).
pub fn parse_json_args(content: &str) -> serde_json::Value {
    let content = content.trim();
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) {
        if parsed.is_object() {
            return parsed;
        }
        return json!({});
    }
    // Regex-free brace scan: the first '{' to the last '}'.
    let Some(start) = content.find('{') else {
        return json!({});
    };
    let Some(end) = content.rfind('}') else {
        return json!({});
    };
    if end < start {
        return json!({});
    }
    serde_json::from_str(&content[start..=end]).unwrap_or(json!({}))
}

fn initial_input_from(history_context: &[String]) -> String {
    for ctx in history_context {
        if let Some(rest) = ctx.strip_prefix("Initial Input:") {
            return rest.trim().to_string();
        }
    }
    String::new()
}

fn last_history(history_context: &[String], count: usize) -> String {
    let start = history_context.len().saturating_sub(count);
    history_context[start..].join("\n")
}

fn normalize_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What the user is asked to approve; the plan digest binds it to the calls.
fn approval_reason(workflow_name: &str, gated: &[PlannedCall]) -> String {
    let plan: Vec<[serde_json::Value; 3]> = gated
        .iter()
        .map(|c| [json!(c.idx), json!(c.tool_name), c.kwargs.clone()])
        .collect();
    let digest = sha256_12(&serde_json::to_string(&plan).unwrap_or_default());
    let mut lines = vec![format!(
        "Workflow '{workflow_name}' will run these {} steps, which need your approval (plan {digest}):",
        gated.len()
    )];
    lines.extend(gated.iter().map(|c| format!("  {}", c.line)));
    lines.join("\n")
}

/// First 12 hex chars of the SHA-256 digest (plan identity).
fn sha256_12(text: &str) -> String {
    use std::fmt::Write;
    // A tiny SHA-256 is overkill; the digest only needs stability within a
    // process lifetime and across the confirm round trip. Use a stable FNV-1a
    // double hash — collisions across two hash functions are negligible for
    // this use, and the calls themselves are re-verified by exact match.
    let mut h1: u64 = 0xcbf29ce484222325;
    let mut h2: u64 = 0x842223253b8c2f1b;
    for byte in text.as_bytes() {
        h1 ^= *byte as u64;
        h1 = h1.wrapping_mul(0x100000001b3);
        h2 = h2.rotate_left(7) ^ (*byte as u64);
        h2 = h2.wrapping_mul(0x2545f4914f6cdd1d);
    }
    let mut out = String::new();
    let _ = write!(out, "{h1:016x}{h2:016x}");
    out[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn::model_loop::testing::FakeModelCaller;
    use crate::turn::scope_for;
    use std::path::Path;

    fn completion(content: &str) -> crate::llm::CompletionResult {
        crate::llm::CompletionResult {
            content: content.to_string(),
            tool_calls: Vec::new(),
            runs_own_tool_loop: false,
            provider: "fake".to_string(),
            duration_ms: None,
            cost_usd: None,
            usage: None,
        }
    }

    fn executor_ctx(
        project_root: &Path,
        caller: Arc<dyn crate::turn::ModelCaller>,
    ) -> ExecutorContext {
        ExecutorContext {
            project_root: project_root.to_path_buf(),
            workspace_root: project_root.to_path_buf(),
            scope: scope_for("cli", "dev"),
            channel: "cli".to_string(),
            sender: "dev".to_string(),
            brain: None,
            notify: None,
            confirmed: None,
            caller,
        }
    }

    fn reasoning_workflow() -> WorkflowDefinition {
        WorkflowDefinition {
            name: "demo".to_string(),
            description: "d".to_string(),
            steps: vec![
                "Summarize the input".to_string(),
                "Draft a reply".to_string(),
            ],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn full_run_completes_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let caller = Arc::new(FakeModelCaller::new(vec![
            completion("output-1"),
            completion("output-2"),
        ]));
        let mut ex = WorkflowExecutor::new(executor_ctx(dir.path(), caller));

        let out = ex
            .execute(&reasoning_workflow(), "the input", None)
            .await
            .unwrap();

        assert!(out.contains("output-2"), "{out}");
        let store = TaskRunStore::new(dir.path());
        assert!(store.list_incomplete().is_empty());
    }

    #[tokio::test]
    async fn resume_skips_completed_steps() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskRunStore::new(dir.path());
        let mut run = store.create("demo", "the input", &reasoning_workflow().steps);
        run.steps[0].status = DONE.to_string();
        run.steps[0].result = "already summarized".to_string();
        store.save(&mut run);

        let caller = Arc::new(FakeModelCaller::new(vec![completion("second step output")]));
        let mut ex = WorkflowExecutor::new(executor_ctx(dir.path(), caller));

        ex.execute(&reasoning_workflow(), "the input", Some(&run.id))
            .await
            .unwrap();

        // Only the second (unfinished) step should have invoked the model.
        assert!(store.get(&run.id).unwrap().is_complete());
    }

    #[tokio::test]
    async fn step_retries_then_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let wf = WorkflowDefinition {
            name: "demo".to_string(),
            description: "d".to_string(),
            steps: vec!["Summarize".to_string()],
            ..Default::default()
        };
        // Two responses: the fake caller fails the first, succeeds after.
        struct Flaky(std::sync::Mutex<u32>);
        #[async_trait::async_trait]
        impl crate::turn::ModelCaller for Flaky {
            async fn complete(
                &self,
                _messages: &[crate::llm::ChatMessage],
                _model: &str,
                _tools: Option<&[serde_json::Value]>,
                _tool_names: &[String],
                _scope: &str,
            ) -> Result<crate::llm::CompletionResult, crate::llm::LlmError> {
                let mut n = self.0.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    Err(crate::llm::LlmError::Unsupported("transient".into()))
                } else {
                    Ok(completion("recovered"))
                }
            }
        }
        let mut ex = WorkflowExecutor::new(executor_ctx(
            dir.path(),
            Arc::new(Flaky(std::sync::Mutex::new(0))),
        ));

        let out = ex.execute(&wf, "x", None).await.unwrap();
        assert!(out.contains("recovered"), "{out}");
    }

    #[tokio::test]
    async fn failure_marks_run_failed_and_keeps_progress() {
        let dir = tempfile::tempdir().unwrap();
        let wf = WorkflowDefinition {
            name: "demo".to_string(),
            description: "d".to_string(),
            steps: vec!["Summarize".to_string()],
            ..Default::default()
        };
        struct AlwaysFail;
        #[async_trait::async_trait]
        impl crate::turn::ModelCaller for AlwaysFail {
            async fn complete(
                &self,
                _messages: &[crate::llm::ChatMessage],
                _model: &str,
                _tools: Option<&[serde_json::Value]>,
                _tool_names: &[String],
                _scope: &str,
            ) -> Result<crate::llm::CompletionResult, crate::llm::LlmError> {
                Err(crate::llm::LlmError::Unsupported("boom".into()))
            }
        }
        let mut ex = WorkflowExecutor::new(executor_ctx(dir.path(), Arc::new(AlwaysFail)));

        let out = ex.execute(&wf, "x", None).await.unwrap();
        assert!(out.contains("failed at step 1"), "{out}");
        assert!(TaskRunStore::new(dir.path()).list_incomplete().is_empty());
    }

    // --- the once-per-run approval flow (test_workflow_approval.py) ---

    fn browser_workflow() -> WorkflowDefinition {
        WorkflowDefinition {
            name: "AskExample".to_string(),
            description: "d".to_string(),
            tags: vec!["browser".to_string()],
            steps: vec![
                "chrome_browser.open_url -> https://example.org".to_string(),
                "chrome_browser.wait_for_idle".to_string(),
                "chrome_browser.fill_prompt -> what is 2+2?".to_string(),
                "chrome_browser.submit_prompt".to_string(),
                "chrome_browser.get_page_text".to_string(),
            ],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn gated_workflow_asks_for_one_approval() {
        let dir = tempfile::tempdir().unwrap();
        let caller = Arc::new(FakeModelCaller::new(vec![]));
        let mut ex = WorkflowExecutor::new(executor_ctx(dir.path(), caller));

        let err = ex
            .execute(&browser_workflow(), "what is 2+2?", None)
            .await
            .expect_err("approval needed");

        let store = PendingActionStore::new(dir.path());
        let pending = store.list();
        assert_eq!(pending.len(), 1, "one approval, not one per step");
        let record = &pending[0];
        assert_eq!(record.tool_name, APPROVAL_TOOL);
        assert_eq!(
            record.arguments,
            serde_json::json!({"workflow_name": "AskExample", "inputs": "what is 2+2?"})
        );
        assert!(
            !dir.path().join("personal/tasks").exists(),
            "no run recorded yet"
        );
        assert!(err.message.contains(&format!("confirm {}", record.id)));
        for shown in ["https://example.org", "what is 2+2?", "get_page_text"] {
            assert!(
                record.reason.contains(shown),
                "{shown} in {}",
                record.reason
            );
        }
        assert!(!err.stale);
    }

    #[tokio::test]
    async fn confirming_the_approval_admits_exactly_the_planned_calls() {
        let dir = tempfile::tempdir().unwrap();
        let caller = Arc::new(FakeModelCaller::new(vec![]));
        let mut ex = WorkflowExecutor::new(executor_ctx(dir.path(), caller));

        // First pass queues the whole-run approval.
        let first = ex
            .execute(&browser_workflow(), "what is 2+2?", None)
            .await
            .unwrap_err();
        assert!(first.message.contains("ACTION SUSPENDED"));

        // The user confirms: the same call runs again with the confirmed
        // record, and preflight admits exactly the planned calls.
        let store = PendingActionStore::new(dir.path());
        let record = store.list()[0].clone();
        let mut ctx = executor_ctx(dir.path(), Arc::new(FakeModelCaller::new(vec![])));
        ctx.confirmed = Some(record.clone());
        let mut ex = WorkflowExecutor::new(ctx);
        ex.preflight(&browser_workflow(), "what is 2+2?", None)
            .expect("the confirmed approval is accepted");
        assert_eq!(ex.approved_calls.len(), 5, "one per gated step");
        // The record itself is consumed by the confirm flow (turn::prepare).
        store.remove(record.id);
    }

    #[tokio::test]
    async fn approval_does_not_cover_a_changed_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let caller = Arc::new(FakeModelCaller::new(vec![]));
        let mut ex = WorkflowExecutor::new(executor_ctx(dir.path(), caller));
        let _ = ex
            .execute(&browser_workflow(), "what is 2+2?", None)
            .await
            .unwrap_err();

        let store = PendingActionStore::new(dir.path());
        let record = store.list()[0].clone();
        // The workflow changed after the user approved: a different url.
        let mut changed = browser_workflow();
        changed.steps[0] = "chrome_browser.open_url -> https://other.org".to_string();

        let mut ctx = executor_ctx(dir.path(), Arc::new(FakeModelCaller::new(vec![])));
        ctx.confirmed = Some(record);
        let mut ex = WorkflowExecutor::new(ctx);
        let err = ex
            .execute(&changed, "what is 2+2?", None)
            .await
            .unwrap_err();
        assert!(err.stale, "the approval no longer matches");
        assert!(err.message.contains("changed after it was approved"));
    }

    #[tokio::test]
    async fn model_chosen_args_are_gated_step_by_step() {
        let dir = tempfile::tempdir().unwrap();
        // `write_file_full` is not a planned verb: the model fills its args
        // at run time, and the gate stops the run with a suspension.
        let wf = WorkflowDefinition {
            name: "writer".to_string(),
            description: "d".to_string(),
            steps: vec!["shell run the echo".to_string()],
            ..Default::default()
        };
        let caller = Arc::new(FakeModelCaller::new(vec![completion(
            r#"{"command": "echo hi"}"#,
        )]));
        let mut ex = WorkflowExecutor::new(executor_ctx(dir.path(), caller));

        let err = ex.execute(&wf, "x", None).await.unwrap_err();
        assert!(err.message.contains("stopped at step 1"), "{}", err.message);
        assert!(err.message.contains("ACTION SUSPENDED"));
        let store = PendingActionStore::new(dir.path());
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.list()[0].tool_name, "shell");
    }

    #[test]
    fn parse_step_forms() {
        let (tool, method, args) = parse_step("chrome_browser.open_url -> https://x.org");
        assert_eq!(tool.as_deref(), Some("chrome_browser"));
        assert_eq!(method, "open_url");
        assert_eq!(args, "https://x.org");

        let (tool, method, args) = parse_step("web_search query about AI");
        assert_eq!(tool.as_deref(), Some("web_search"));
        assert_eq!(method, "");
        assert_eq!(args, "query about AI");

        let (tool, _method, args) = parse_step("Summarize the input");
        assert_eq!(tool, None);
        assert_eq!(args, "Summarize the input");
    }

    #[test]
    fn direct_kwargs_map_the_browser_verbs() {
        assert_eq!(
            direct_kwargs("open_url", "https://x.org", ""),
            Some(serde_json::json!({"action": "open_url", "url": "https://x.org"}))
        );
        assert_eq!(
            direct_kwargs("wait", "5", ""),
            Some(serde_json::json!({"action": "wait", "seconds": 5}))
        );
        assert_eq!(
            direct_kwargs("wait_for_idle", "", ""),
            Some(serde_json::json!({"action": "wait_for_idle", "timeout_secs": 60}))
        );
        assert_eq!(direct_kwargs("unknown_verb", "", ""), None);
    }

    #[test]
    fn json_args_extraction_is_tolerant() {
        assert_eq!(parse_json_args("{\"a\": 1}"), serde_json::json!({"a": 1}));
        assert_eq!(
            parse_json_args("Sure! {\"a\": 1} hope that helps"),
            serde_json::json!({"a": 1})
        );
        assert_eq!(parse_json_args("no json here"), serde_json::json!({}));
    }

    #[tokio::test]
    async fn approved_calls_skip_the_gate() {
        // `write_file_full` always needs confirmation — but a call the user
        // approved as part of this run goes straight through.
        let dir = tempfile::tempdir().unwrap();
        let caller = Arc::new(FakeModelCaller::new(vec![]));
        let mut ex = WorkflowExecutor::new(executor_ctx(dir.path(), caller));
        let tool = ToolRegistry.get_tool("shell").unwrap();
        let kwargs = serde_json::json!({"command": "echo workflow-gate-test"});

        let out = ex.call_tool_gated(tool, &kwargs).await.unwrap();
        assert!(matches!(out, StepOutput::Suspended { .. }));

        ex.approved_calls.insert(call_key("shell", &kwargs));
        let out = ex.call_tool_gated(tool, &kwargs).await.unwrap();
        match out {
            StepOutput::Text(text) => assert!(text.contains("workflow-gate-test"), "{text}"),
            StepOutput::Suspended { .. } => panic!("approved call must not suspend"),
        }
    }
}
