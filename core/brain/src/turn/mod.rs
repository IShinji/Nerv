//! One user turn as the Rust facade drives it: the port of the orchestrator's
//! `dispatch`/`_dispatch` flow, with the model loop in
//! [`crate::turn::model_loop`] and python kept for the leaf operations that
//! have not migrated (memory context, agent registry, tool execution, the
//! pending store) behind thin RPCs:
//!
//! - `prepare_turn` — review-queue commands, the direct-reply path, agent
//!   selection, workflow fallback, context assembly, tool schemas, and the
//!   pending snapshot
//! - `run_tool` — capability resolution, the confirmation gate, execution
//! - `finish_turn` — new-pending surfacing, memory append, summary backfill
//!
//! The trace line is written here (Rust owns the timing); scope binding stays
//! python-side inside each handler.

pub mod model_loop;
mod pending;
mod trace;

#[cfg(test)]
mod tests;

pub use model_loop::{ModelCaller, ProviderCaller};
pub use pending::PendingRecord;
pub use trace::TraceRecord;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nerv_shared::brain_client::{BrainClient, BrainError};
use serde_json::json;

use crate::router::RouteResult;
use model_loop::{run_model_loop, LoopContext, TurnError};

/// Filesystem-safe conversation scope. Port of `scope_for` in
/// `brain/nerv/memory/manager.py` (shared with the python side, which derives
/// the same value from channel+sender).
pub fn scope_for(channel: &str, sender: &str) -> String {
    const DEFAULT_SCOPE: &str = "default";
    let parts: Vec<String> = [channel, sender]
        .iter()
        .filter_map(|part| {
            let trimmed = part.trim().to_lowercase();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .collect();
    if parts.is_empty() {
        return DEFAULT_SCOPE.to_string();
    }
    let joined = parts.join("-");
    let slug: String = joined
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        DEFAULT_SCOPE.to_string()
    } else {
        slug
    }
}

/// Drives one user turn end to end.
pub struct TurnRunner {
    brain: Arc<BrainClient>,
    project_root: PathBuf,
    max_rounds: u32,
}

impl TurnRunner {
    /// Build a runner; `max_rounds` mirrors
    /// `orchestrator.max_tool_iterations` from config (python re-read it per
    /// turn, the rust facade reads it once at startup).
    pub fn new(brain: Arc<BrainClient>, project_root: &Path) -> Self {
        let max_rounds = nerv_shared::config::NervConfig::load(project_root)
            .map(|config| config.orchestrator.max_tool_iterations)
            .unwrap_or(20);
        Self {
            brain,
            project_root: project_root.to_path_buf(),
            max_rounds,
        }
    }

    #[cfg(test)]
    pub fn with_max_rounds(brain: Arc<BrainClient>, project_root: &Path, max_rounds: u32) -> Self {
        Self {
            brain,
            project_root: project_root.to_path_buf(),
            max_rounds,
        }
    }

    /// Run one turn and return the reply text.
    ///
    /// Mirrors `Orchestrator.dispatch`: the trace line is written once the
    /// reply is decided (or on failure), and can never fail the request.
    pub async fn run_turn(
        &self,
        message: &str,
        route_result: &RouteResult,
        channel: &str,
        sender: &str,
        caller: std::sync::Arc<dyn ModelCaller>,
    ) -> Result<String, BrainError> {
        let scope = scope_for(channel, sender);
        let mut trace = TraceRecord::new(channel, &scope);
        trace.tier = Some(route_result.model_tier);

        let outcome: Result<String, TurnError> = async {
            // Everything before the model call, on the python side.
            let prepared = self
                .brain
                .call(
                    "prepare_turn",
                    json!({
                        "message": message,
                        "route_result": serde_json::to_value(route_result)
                            .expect("serializes"),
                        "channel": channel,
                        "sender": sender,
                    }),
                )
                .await?;

            match prepared["status"].as_str().unwrap_or("") {
                // Handled without any model call (review queue, direct router
                // reply, fallback workflow, a late confirm/cancel).
                "review" | "direct" | "workflow_fallback" | "pending" => {
                    self.finish_simple(message, channel, sender, &prepared, &mut trace)
                        .await
                }
                "agent" => {
                    self.run_agent_turn(
                        message,
                        route_result,
                        channel,
                        sender,
                        &scope,
                        &prepared,
                        caller.as_ref(),
                        caller.clone(),
                        &mut trace,
                    )
                    .await
                }
                other => Err(TurnError::Brain(BrainError::Rpc(format!(
                    "prepare_turn returned unknown status {other:?}"
                )))),
            }
        }
        .await;

        match outcome {
            Ok(reply) => {
                trace.write(&self.project_root);
                Ok(reply)
            }
            Err(e) => {
                trace.error = error_kind(&e);
                trace.write(&self.project_root);
                Err(match e {
                    TurnError::Brain(e) => e,
                    TurnError::Llm(e) => BrainError::Rpc(e.to_string()),
                })
            }
        }
    }

    /// Reply paths decided without a model call still need the assistant
    /// message remembered and the summary backfill scheduled.
    async fn finish_simple(
        &self,
        message: &str,
        channel: &str,
        sender: &str,
        prepared: &serde_json::Value,
        trace: &mut TraceRecord,
    ) -> Result<String, TurnError> {
        let scope = scope_for(channel, sender);
        let memory = Arc::new(crate::memory::MemoryManager::new(
            &self.project_root,
            &scope,
        ));
        memory.append_message("user", message);
        let reply = prepared["reply"].as_str().unwrap_or("").to_string();
        let notifications = prepared["notifications"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let finished = self
            .brain
            .call(
                "finish_turn",
                json!({
                    "message": message,
                    "reply": reply,
                    "channel": channel,
                    "sender": sender,
                    "known_pending": [],
                    "notifications": notifications,
                    "pending_created": false,
                }),
            )
            .await?;
        if finished["pending_created"].as_bool().unwrap_or(false) {
            trace.pending_created = true;
        }
        let final_reply = finished["reply"].as_str().unwrap_or("").to_string();
        memory.append_message("assistant", &final_reply);
        Ok(final_reply)
    }

    /// The agent path: run the model loop, then let python finalize the turn.
    #[allow(clippy::too_many_arguments)]
    async fn run_agent_turn(
        &self,
        message: &str,
        route_result: &RouteResult,
        channel: &str,
        sender: &str,
        scope: &str,
        prepared: &serde_json::Value,
        caller: &dyn ModelCaller,
        caller_arc: crate::memory::summarizer::ArcCaller,
        trace: &mut TraceRecord,
    ) -> Result<String, TurnError> {
        let agent = &prepared["agent"];
        let agent_name = agent["name"].as_str().unwrap_or("General").to_string();
        let agent_tools: Vec<String> = agent["tools"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let model = prepared["model"].as_str().unwrap_or_default().to_string();
        let known_pending: Vec<PendingRecord> =
            serde_json::from_value(prepared["known_pending"].clone()).unwrap_or_default();

        // Memory is the facade's now: persist the user's message, then let the
        // context manager assemble the prompt window (facts, summaries,
        // budgeted history) around it.
        let memory = Arc::new(crate::memory::MemoryManager::new(&self.project_root, scope));
        memory.append_message("user", message);
        let agent_prompt = crate::memory::AgentPrompt {
            name: agent_name.clone(),
            system_prompt: agent["system_prompt"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            max_context_tokens: agent["max_context_tokens"].as_i64().unwrap_or(8000),
        };
        let catalogs: Vec<String> = prepared["catalogs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let messages = crate::memory::ContextManager { memory: &memory }.build_messages(
            &agent_prompt,
            message,
            10,
            &catalogs,
        );
        let tools_schemas: Option<Vec<serde_json::Value>> =
            serde_json::from_value(prepared["tools_schemas"].clone()).ok();

        let (loop_outcome, notifications) = {
            let mut ctx = LoopContext {
                brain: &self.brain,
                caller,
                trace,
                max_rounds: self.max_rounds,
                sender,
                channel,
                scope,
                memory: memory.clone(),
                pending_notifications: Vec::new(),
            };
            let outcome = run_model_loop(
                &mut ctx,
                &agent_name,
                &model,
                messages,
                tools_schemas.as_deref(),
                &agent_tools,
                &route_result.intent,
                route_result.model_tier,
            )
            .await;
            (outcome, std::mem::take(&mut ctx.pending_notifications))
        };
        // Graceful degradation: a model failure surfaces a clean, user-facing
        // message rather than a stack trace (port of the _dispatch catch).
        let reply = match loop_outcome {
            Ok(reply) => reply,
            Err(e) => {
                tracing::error!("Model call failed: {e}");
                trace.error = error_kind(&e);
                "I hit a problem reaching the model and couldn't complete that \
                 just now. Please try again in a moment."
                    .to_string()
            }
        };

        let finished = self
            .brain
            .call(
                "finish_turn",
                json!({
                    "reply": reply,
                    "channel": channel,
                    "sender": sender,
                    "known_pending": known_pending,
                    "notifications": notifications,
                }),
            )
            .await?;
        if finished["pending_created"].as_bool().unwrap_or(false) {
            trace.pending_created = true;
        }
        let final_reply = finished["reply"].as_str().unwrap_or("").to_string();
        // The finalized reply (with any pending additions) is what memory
        // keeps, matching the old python order.
        memory.append_message("assistant", &final_reply);
        let tier0_model = nerv_shared::config::NervConfig::load(&self.project_root)
            .map(|config| config.models.tiers.tier0)
            .unwrap_or_else(|_| "claude-cli:opus".to_string());
        crate::memory::summarizer::spawn_backfill(
            self.project_root.clone(),
            tier0_model,
            caller_arc,
        );
        Ok(final_reply)
    }
}

fn error_kind(error: &TurnError) -> String {
    // Python logged the exception type name; use the closest stable analogue.
    match error {
        TurnError::Llm(_) => "LlmError".to_string(),
        TurnError::Brain(_) => "BrainError".to_string(),
    }
}
