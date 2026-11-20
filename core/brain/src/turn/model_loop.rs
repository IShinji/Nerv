//! The agent-turn model loop: call the model, execute requested tools (via
//! the python `run_tool` RPC), feed results back, stop on a final answer or
//! the round cap. Port of `_call_model` in
//! `brain/nerv/orchestrator/orchestrator.py`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use nerv_shared::brain_client::BrainError;
use serde_json::json;
use tracing::{info, warn};

use crate::llm::claude_cli::ClaudeCliBackend;
use crate::llm::{self, ChatMessage, CompleteOptions, CompletionResult, LlmError, ToolCall};
use crate::turn::pending::{build_pending_action_reply, PendingRecord};
use crate::turn::trace::{estimate_tokens, log_event, new_trace_id, TraceRecord};

pub const EMPTY_RESPONSE_MESSAGE: &str = "I received an empty response from the model.";

/// One model call for the loop. Production routes by provider prefix
/// (claude-cli/local run in Rust, litellm stays on python until that backend
/// is ported); tests inject a scripted fake.
// async_trait expands the method into a `#[must_use]` boxed future that
// clippy flags as redundant with the `Result` return; not fixable at source.
pub use crate::llm::ProgressCallback;

// async_trait expands the method into a `#[must_use]` boxed future that
// clippy flags as redundant with the `Result` return; not fixable at source.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait ModelCaller: Send + Sync {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        model: &str,
        tools: Option<&[serde_json::Value]>,
        tool_names: &[String],
        scope: &str,
        progress: Option<&ProgressCallback>,
    ) -> Result<CompletionResult, LlmError>;
}

/// Production caller.
pub struct ProviderCaller {
    pub claude_cli_backend: ClaudeCliBackend,
    pub project_root: PathBuf,
}

#[async_trait::async_trait]
impl ModelCaller for ProviderCaller {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        model: &str,
        tools: Option<&[serde_json::Value]>,
        tool_names: &[String],
        scope: &str,
        progress: Option<&ProgressCallback>,
    ) -> Result<CompletionResult, LlmError> {
        // Every provider — claude-cli, local ollama and the multi-vendor
        // litellm cloud family — runs natively.
        llm::complete_with_progress(
            messages,
            &CompleteOptions {
                model,
                tools,
                tool_names,
                temperature: 0.2,
                json_mode: false,
                project_root: Some(&self.project_root),
                scope,
                timeout: Duration::from_secs(120),
            },
            progress,
        )
        .await
    }
}

/// Everything one turn's model loop needs from its host.
pub struct LoopContext<'a> {
    pub caller: &'a dyn ModelCaller,
    /// The 流式推送 typewriter feed, wired to the originating adapter.
    pub progress: Option<&'a ProgressCallback>,
    pub trace: &'a mut TraceRecord,
    pub max_rounds: u32,
    pub sender: &'a str,
    pub channel: &'a str,
    pub scope: &'a str,
    pub project_root: &'a Path,
    /// Shared with the finish phase, which remembers the finalized reply.
    pub memory: std::sync::Arc<crate::memory::MemoryManager>,
    /// Background-alert lines accumulated during the turn (suspended actions
    /// queued by the MCP server); drained by the heartbeat afterwards.
    pub pending_notifications: Vec<String>,
}

/// Errors that abort the model loop (the caller degrades gracefully).
#[derive(Debug, thiserror::Error)]
pub enum TurnError {
    #[error("{0}")]
    Llm(#[from] LlmError),
    #[error("{0}")]
    Brain(#[from] BrainError),
}

/// Run the tool loop and return the final reply text.
///
/// Mirrors `_call_model`: the claude-cli backend drives its own tool loop
/// internally (one call = final answer), while ollama/litellm round-trip
/// through `run_tool` until the model stops requesting tools or the round
/// cap is hit.
#[allow(clippy::too_many_arguments)]
pub async fn run_model_loop(
    ctx: &mut LoopContext<'_>,
    agent_name: &str,
    model: &str,
    mut messages: Vec<ChatMessage>,
    tools_schemas: Option<&[serde_json::Value]>,
    agent_tools: &[String],
    intent: &str,
    tier: i64,
) -> Result<String, TurnError> {
    if ctx.trace.model.is_empty() {
        ctx.trace.model = model.to_string();
    }
    ctx.trace.est_input_tokens = estimate_tokens(&messages);

    let started = std::time::Instant::now();
    let event_fields = json!({
        "trace_id": new_trace_id(),
        "channel": ctx.channel,
        "intent": intent,
        "agent": agent_name,
        "tier": tier,
        "model": model,
    });
    let outcome = loop_body(ctx, model, &mut messages, tools_schemas, agent_tools).await;
    let duration_ms = round1(started.elapsed().as_secs_f64() * 1000.0);
    let mut event = event_fields;
    match &outcome {
        Ok(_) => {
            event["ok"] = json!(true);
            event["duration_ms"] = json!(duration_ms);
        }
        Err(e) => {
            event["ok"] = json!(false);
            event["duration_ms"] = json!(duration_ms);
            event["error"] = json!(format!("{e}"));
        }
    }
    log_event("turn", event);
    outcome
}

async fn loop_body(
    ctx: &mut LoopContext<'_>,
    model: &str,
    messages: &mut Vec<ChatMessage>,
    tools_schemas: Option<&[serde_json::Value]>,
    agent_tools: &[String],
) -> Result<String, TurnError> {
    let max_rounds = ctx.max_rounds.max(1);
    for _round in 0..max_rounds {
        let result = ctx
            .caller
            .complete(
                messages,
                model,
                tools_schemas,
                agent_tools,
                ctx.scope,
                ctx.progress,
            )
            .await
            .map_err(TurnError::from)?;

        if !result.provider.is_empty() {
            ctx.trace.provider = result.provider.clone();
        }
        if let Some(cost) = result.cost_usd {
            ctx.trace.cost_usd = Some(ctx.trace.cost_usd.unwrap_or(0.0) + cost);
        }

        let mut assistant = ChatMessage::assistant(result.content.clone());
        if !result.tool_calls.is_empty() {
            assistant.tool_calls = Some(result.tool_calls.clone());
        }
        messages.push(assistant);

        // claude-cli already executed any tools internally via MCP.
        if result.runs_own_tool_loop {
            return Ok(if result.content.is_empty() {
                EMPTY_RESPONSE_MESSAGE.to_string()
            } else {
                result.content
            });
        }

        if !result.tool_calls.is_empty() {
            info!("Model requested {} tool calls", result.tool_calls.len());
            let queued = execute_tool_calls(ctx, messages, &result.tool_calls).await?;
            if !queued.is_empty() {
                return Ok(build_pending_action_reply(&queued));
            }
            // Loop continues, querying the model with the new tool context.
            continue;
        }

        return Ok(if result.content.is_empty() {
            EMPTY_RESPONSE_MESSAGE.to_string()
        } else {
            result.content
        });
    }

    warn!("Tool loop stopped after {max_rounds} rounds");
    Ok(format!(
        "I stopped after {max_rounds} rounds of tool calls without reaching \
         a final answer. Try a narrower request, or raise \
         orchestrator.max_tool_iterations in the config."
    ))
}

/// Execute one round's tool calls — natively for the tools the rust brain
/// has, through the python `run_tool` RPC for the rest — and append one tool
/// message per call. Returns the actions that were suspended for
/// confirmation, if any.
async fn execute_tool_calls(
    ctx: &mut LoopContext<'_>,
    messages: &mut Vec<ChatMessage>,
    tool_calls: &[ToolCall],
) -> Result<Vec<PendingRecord>, TurnError> {
    let mut queued: Vec<PendingRecord> = Vec::new();
    for tc in tool_calls {
        let func_name = tc.function.name.clone();
        ctx.trace.tools_called.push(func_name.clone());

        // Every registered tool executes natively; an unregistered name is a
        // model hallucination and is reported back as such (the old python
        // run_tool RPC did the same through its registry).
        let response = match native_run_tool(ctx, &func_name, &tc.function.arguments).await {
            Ok(Some(response)) => response,
            Err(e) => return Err(TurnError::Brain(e)),
            Ok(None) => serde_json::json!({
                "content": format!("Unknown tool: {func_name}"),
                "suspended": false,
            }),
        };

        let suspended = response["suspended"].as_bool().unwrap_or(false);
        let content = response["content"].as_str().unwrap_or("").to_string();
        if suspended {
            if let Some(record) = response.get("pending").map(|p| {
                serde_json::from_value::<PendingRecord>(p.clone()).expect("run_tool pending shape")
            }) {
                ctx.trace.pending_created = true;
                if let Some(notification) = response.get("notification").and_then(|v| v.as_str()) {
                    ctx.pending_notifications.push(notification.to_string());
                }
                warn!("Tool {} suspended for confirmation", record.tool_name);
                queued.push(record);
            }
        }
        messages.push(ChatMessage::tool(content));
    }
    Ok(queued)
}

/// Run a tool natively when the rust registry has it; `None` hands the call
/// to python. The confirmation gate (sandbox policy) runs on both sides for
/// their respective tools.
async fn native_run_tool(
    ctx: &LoopContext<'_>,
    name: &str,
    arguments: &serde_json::Value,
) -> Result<Option<serde_json::Value>, BrainError> {
    let registry = crate::tools::ToolRegistry;
    let Some(tool) = registry.get_tool(name) else {
        return Ok(None);
    };

    let workspace_root = crate::tools::facade_workspace_root(ctx.project_root);
    let Some(reason) = crate::tools::sandbox::confirmation_reason(
        name,
        arguments,
        &workspace_root,
        tool.requires_confirmation,
        Some(ctx.project_root),
    ) else {
        // Execute natively.
        let tool_ctx = crate::tools::ToolContext {
            project_root: ctx.project_root.to_path_buf(),
            workspace_root,
            scope: ctx.scope.to_string(),
            brain: None,
            confirmed: None,
        };
        let result = (tool.func)(&tool_ctx, arguments).await;
        return Ok(Some(json!({
            "content": result.content,
            "suspended": false,
        })));
    };

    // Suspended: queue it in the shared file-backed store (the python
    // confirm/cancel flow reads the same file).
    let store = crate::tools::pending_store::PendingActionStore::new(ctx.project_root);
    let record = match store.add(
        tool.name,
        arguments.clone(),
        &reason,
        ctx.sender,
        ctx.channel,
        ctx.scope,
        "",
    ) {
        Ok(record) => record,
        Err(e) => return Err(BrainError::Rpc(format!("pending store failed: {e}"))),
    };
    let summary = crate::tools::pending_store_format(&record);
    let notification = format!(
        "⚠️ [SYSTEM BACKGROUND ALERT] Pending action #{} ({}) is waiting for confirmation.",
        record.id, record.tool_name
    );
    let response = json!({
        "content": format!("[ACTION SUSPENDED] {summary}"),
        "suspended": true,
        "pending": serde_json::to_value(&record).expect("serializes"),
        "notification": notification,
    });
    Ok(Some(response))
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// A scripted caller: pops one prepared result per call and records every
    /// message list it was handed, so tests can assert the loop's behavior.
    pub struct FakeModelCaller {
        responses: Mutex<VecDeque<CompletionResult>>,
        pub calls: Mutex<Vec<Vec<ChatMessage>>>,
    }

    impl FakeModelCaller {
        pub fn new(responses: Vec<CompletionResult>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                calls: Mutex::new(Vec::new()),
            }
        }

        pub fn recorded_calls(&self) -> Vec<Vec<ChatMessage>> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl ModelCaller for FakeModelCaller {
        async fn complete(
            &self,
            messages: &[ChatMessage],
            _model: &str,
            _tools: Option<&[serde_json::Value]>,
            _tool_names: &[String],
            _scope: &str,
            _progress: Option<&ProgressCallback>,
        ) -> Result<CompletionResult, LlmError> {
            self.calls.lock().unwrap().push(messages.to_vec());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| LlmError::Unsupported("no scripted response".into()))
        }
    }
}
