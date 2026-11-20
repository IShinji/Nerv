//! One user turn as the Rust facade drives it: the port of the orchestrator's
//! `dispatch`/`_dispatch` flow, with the model loop in
//! [`crate::turn::model_loop`]. The pre-model stages and finalization run
//! locally in [`prepare`] (pending commands, review queue, agent selection,
//! model selection, new-pending surfacing); python keeps only the leaf
//! operations that have not migrated, behind thin RPCs:
//!
//! - `run_tool` — capability resolution, the confirmation gate, execution
//!   for the tools that are still python
//! - `workflow_fallback` — the WorkflowExecutor is still python
//! - `execute_pending` — confirming a queued python-only tool
//! - `complete_turn` — python-only model backends (litellm)
//!
//! The trace line is written here (Rust owns the timing).

pub mod model_loop;
pub mod pending;
pub mod prepare;
mod trace;

#[cfg(test)]
mod tests;

pub use model_loop::{ModelCaller, ProviderCaller};
pub use pending::{format_pending_action, PendingRecord};
pub use trace::TraceRecord;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nerv_shared::brain_client::BrainError;

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
    project_root: PathBuf,
    max_rounds: u32,
    /// Vector retrieval opt-in (PRD v2 E10), from `brain.vector_retrieval`.
    vector_retrieval: bool,
}

impl TurnRunner {
    /// Build a runner; `max_rounds` mirrors
    /// `orchestrator.max_tool_iterations` from config (python re-read it per
    /// turn, the rust facade reads it once at startup).
    pub fn new(project_root: &Path) -> Self {
        let config = nerv_shared::config::NervConfig::load(project_root).ok();
        let max_rounds = config
            .as_ref()
            .map(|c| c.orchestrator.max_tool_iterations)
            .unwrap_or(20);
        let vector_retrieval = config
            .as_ref()
            .map(|c| c.brain.vector_retrieval)
            .unwrap_or(false);
        Self {
            project_root: project_root.to_path_buf(),
            max_rounds,
            vector_retrieval,
        }
    }

    #[cfg(test)]
    pub fn with_max_rounds(project_root: &Path, max_rounds: u32) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            max_rounds,
            vector_retrieval: false,
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
        progress: Option<&crate::llm::ProgressCallback>,
    ) -> Result<(String, serde_json::Value), BrainError> {
        let scope = scope_for(channel, sender);
        let mut trace = TraceRecord::new(channel, &scope);
        trace.tier = Some(route_result.model_tier);

        let outcome: Result<(String, serde_json::Value), TurnError> = async {
            // Everything before the model call, decided locally (pending
            // commands, review queue, agent selection, model selection).
            let prepared =
                prepare::prepare_turn(&self.project_root, message, route_result, channel, sender)
                    .await?;

            match prepared {
                // Handled without any model call (review queue, direct router
                // reply, fallback workflow, a late confirm/cancel).
                prepare::PreparedTurn::Direct(reply) => {
                    self.finish_simple(message, channel, sender, &reply, &mut trace)
                        .await
                }
                prepare::PreparedTurn::Agent {
                    agent,
                    system_prompt,
                    model,
                    catalogs,
                    tools_schemas,
                    known_pending,
                } => {
                    self.run_agent_turn(
                        message,
                        route_result,
                        channel,
                        sender,
                        &scope,
                        agent,
                        system_prompt,
                        model,
                        catalogs,
                        tools_schemas,
                        known_pending,
                        caller.as_ref(),
                        caller.clone(),
                        &mut trace,
                        progress,
                    )
                    .await
                }
            }
        }
        .await;

        match outcome {
            Ok((reply, meta)) => {
                trace.write(&self.project_root);
                Ok((reply, meta))
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
        reply: &str,
        trace: &mut TraceRecord,
    ) -> Result<(String, serde_json::Value), TurnError> {
        let scope = scope_for(channel, sender);
        let memory = Arc::new(crate::memory::MemoryManager::manager_for(
            &self.project_root,
            &scope,
            self.vector_retrieval,
        ));
        memory.append_message("user", message);
        let (final_reply, pending_created) =
            prepare::finish_turn(&self.project_root, reply, sender, channel, &[]);
        if pending_created {
            trace.pending_created = true;
        }
        memory.append_message("assistant", &final_reply);
        Ok((final_reply, serde_json::json!({})))
    }

    /// The agent path: run the model loop, then finalize the turn locally.
    #[allow(clippy::too_many_arguments)]
    async fn run_agent_turn(
        &self,
        message: &str,
        route_result: &RouteResult,
        channel: &str,
        sender: &str,
        scope: &str,
        agent: crate::orchestrator::AgentDefinition,
        system_prompt: String,
        model: String,
        catalogs: Vec<String>,
        tools_schemas: Option<Vec<serde_json::Value>>,
        known_pending: Vec<PendingRecord>,
        caller: &dyn ModelCaller,
        caller_arc: crate::memory::summarizer::ArcCaller,
        trace: &mut TraceRecord,
        progress: Option<&crate::llm::ProgressCallback>,
    ) -> Result<(String, serde_json::Value), TurnError> {
        let agent_name = agent.name.clone();
        let agent_tools = agent.tools.clone();

        // Memory is the facade's now: persist the user's message, then let the
        // context manager assemble the prompt window (facts, summaries,
        // budgeted history) around it.
        let memory = Arc::new(crate::memory::MemoryManager::manager_for(
            &self.project_root,
            scope,
            self.vector_retrieval,
        ));
        memory.append_message("user", message);
        let agent_prompt = crate::memory::AgentPrompt {
            name: agent_name.clone(),
            system_prompt,
            max_context_tokens: agent.max_context_tokens as i64,
        };
        let messages = crate::memory::ContextManager { memory: &memory }.build_messages(
            &agent_prompt,
            message,
            10,
            &catalogs,
        );

        let (loop_outcome, notifications) = {
            let mut ctx = LoopContext {
                caller,
                progress,
                trace,
                max_rounds: self.max_rounds,
                sender,
                channel,
                scope,
                project_root: &self.project_root,
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
        let (reply, turn_meta) = match loop_outcome {
            Ok((reply, meta)) => (reply, meta),
            Err(e) => {
                tracing::error!("Model call failed: {e}");
                trace.error = error_kind(&e);
                (
                    "I hit a problem reaching the model and couldn't complete that \
                     just now. Please try again in a moment."
                        .to_string(),
                    serde_json::json!({}),
                )
            }
        };

        // Background alerts queued during the turn (suspended actions) reach
        // the heartbeat drain through the notification queue, as before.
        if !notifications.is_empty() {
            crate::proactive::queue_notifications(&notifications);
        }

        let (final_reply, pending_created) =
            prepare::finish_turn(&self.project_root, &reply, sender, channel, &known_pending);
        if pending_created {
            trace.pending_created = true;
        }
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
        Ok((final_reply, turn_meta))
    }
}

fn error_kind(error: &TurnError) -> String {
    // Python logged the exception type name; use the closest stable analogue.
    match error {
        TurnError::Llm(_) => "LlmError".to_string(),
        TurnError::Brain(_) => "BrainError".to_string(),
    }
}
