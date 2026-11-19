//! The turn's pre-model and finalization stages, ported from the python
//! orchestrator (`prepare_turn` / `finish_turn` / pending commands).
//!
//! Everything here runs without the python sidecar: pending-action commands
//! (`confirm <id>` / `cancel <id>`), review-queue commands, agent selection
//! (with the factory fallback), model selection and the new-pending surfacing.
//! The workflow fallback gate stays a thin RPC (`workflow_fallback`) because
//! the WorkflowExecutor is still python (E9b transitional decision).

use std::collections::HashSet;
use std::path::Path;

use regex::Regex;
use serde_json::json;
use tracing::{error, info};

use super::pending::format_pending_action;
use super::scope_for;
use super::PendingRecord;
use crate::orchestrator::{AgentDefinition, AgentFactory, AgentRegistry};
use crate::tools::facade_workspace_root;
use crate::tools::pending_store::PendingActionStore;
use crate::tools::registry::ToolContext;
use crate::workflows::{ReviewQueue, WorkflowRegistry};
use nerv_shared::brain_client::{BrainClient, BrainError};

const CONFIRM_COMMANDS: [&str; 6] = ["confirm", "approve", "run", "yes", "y", "ok"];
const CANCEL_COMMANDS: [&str; 6] = ["cancel", "deny", "reject", "no", "n", "stop"];
const CASUAL_WORDS: [&str; 7] = ["yes", "y", "ok", "no", "n", "stop", "run"];

/// Everything `run_turn` needs before the model loop, decided locally.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum PreparedTurn {
    /// Handled without any model call — a pending command, a review-queue
    /// command, the router's direct reply, or a fallback workflow.
    Direct(String),
    /// The agent path carries the model loop's inputs.
    Agent {
        agent: AgentDefinition,
        system_prompt: String,
        model: String,
        catalogs: Vec<String>,
        tools_schemas: Option<Vec<serde_json::Value>>,
        known_pending: Vec<PendingRecord>,
    },
}

/// Consult the pending queue for `confirm <id>` / `cancel <id>`.
/// `None` means the message is not a pending command and routing continues.
/// `brain` carries the live sidecar connection for python-only tools.
pub async fn handle_pending_command(
    project_root: &Path,
    brain: &BrainClient,
    message: &str,
    sender: &str,
    channel: &str,
) -> Result<Option<String>, BrainError> {
    let store = PendingActionStore::new(project_root);
    let visible = visible_pending_actions(&store.list(), sender, channel);
    if visible.is_empty() {
        return Ok(None);
    }

    let normalized = message
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.is_empty() {
        return Ok(None);
    }

    let command_re =
        Regex::new(r"^(confirm|approve|run|yes|y|ok|cancel|deny|reject|no|n|stop)(?:\s+#?(\d+))?$")
            .expect("static regex");
    let Some(captures) = command_re.captures(&normalized) else {
        return Ok(None);
    };
    let command = captures.get(1).unwrap().as_str();
    let action_id = captures.get(2).and_then(|m| m.as_str().parse::<i64>().ok());

    // Plain chat words ("yes", "ok", "no", "stop") must never run or cancel
    // a queued action, and running one always names its id.
    if CONFIRM_COMMANDS.contains(&command) && action_id.is_none() {
        return Ok(None);
    }
    if CANCEL_COMMANDS.contains(&command) && action_id.is_none() && CASUAL_WORDS.contains(&command)
    {
        return Ok(None);
    }

    let target = match action_id {
        Some(id) => match visible.iter().find(|action| action.id == id) {
            Some(action) => action.clone(),
            None => {
                return Ok(Some(format!("I could not find pending action #{id}.")));
            }
        },
        None if visible.len() == 1 => visible[0].clone(),
        None => {
            let ids = visible
                .iter()
                .map(|action| action.id.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Ok(Some(format!(
                "Multiple pending actions are waiting: {ids}. Reply with confirm <id> or cancel <id>."
            )));
        }
    };

    if CANCEL_COMMANDS.contains(&command) {
        let _ = store.remove(target.id);
        return Ok(Some(format!(
            "Cancelled pending action #{}: {}",
            target.id,
            format_pending_action(&target)
        )));
    }

    if CONFIRM_COMMANDS.contains(&command) {
        return Ok(Some(
            execute_pending_action(project_root, brain, &target).await,
        ));
    }
    Ok(None)
}

/// Run a previously approved pending action. Native tools execute in the
/// Rust registry; python-only tools go through the `execute_pending` RPC.
async fn execute_pending_action(
    project_root: &Path,
    brain: &BrainClient,
    action: &PendingRecord,
) -> String {
    let registry = crate::tools::ToolRegistry;
    let summary = format_pending_action(action);

    // The approval is consumed up front, like the python flow — a failing
    // execution must not leave the action queued forever.
    let _ = PendingActionStore::new(project_root).remove(action.id);

    let result = match registry.get_tool(&action.tool_name) {
        Some(tool) => {
            let workspace_root = facade_workspace_root(project_root);
            let tool_ctx = ToolContext {
                project_root: project_root.to_path_buf(),
                workspace_root,
                scope: action.scope.clone(),
                brain: None,
            };
            let result = (tool.func)(&tool_ctx, &action.arguments).await;
            result.content
        }
        None => match execute_pending_via_python(brain, action).await {
            Ok(content) => content,
            Err(e) => {
                error!("execute_pending RPC failed for #{}: {e}", action.id);
                format!(
                    "Pending action #{} could not be executed because tool {} is unavailable.",
                    action.id, action.tool_name
                )
            }
        },
    };

    format!(
        "Executed pending action #{}: {}\n{}",
        action.id, summary, result
    )
}

/// The confirmation gate is skipped: the user just approved this action.
/// (Python wraps the call in `executing_confirmed(action)` so its tools
/// recognise the approved call.)
async fn execute_pending_via_python(
    brain: &BrainClient,
    action: &PendingRecord,
) -> Result<String, BrainError> {
    let response = brain
        .call(
            "execute_pending",
            json!({
                "id": action.id,
                "tool_name": action.tool_name,
                "arguments": action.arguments,
                "justification": action.justification,
            }),
        )
        .await?;
    Ok(response["content"].as_str().unwrap_or("").to_string())
}

/// Pending actions that belong to exactly this sender/channel (port of
/// `_visible_pending_actions`). Unscoped actions are visible only to an
/// unscoped caller such as the local CLI, never to a remote sender.
pub fn visible_pending_actions(
    actions: &[PendingRecord],
    sender: &str,
    channel: &str,
) -> Vec<PendingRecord> {
    if sender.is_empty() && channel.is_empty() {
        return actions
            .iter()
            .filter(|a| a.sender.is_empty() && a.channel.is_empty() && a.scope.is_empty())
            .cloned()
            .collect();
    }
    let wanted = scope_for(channel, sender);
    actions
        .iter()
        .filter(|a| {
            (!a.scope.is_empty() && a.scope == wanted)
                || (a.scope.is_empty()
                    && (!a.sender.is_empty() || !a.channel.is_empty())
                    && a.sender == sender
                    && a.channel == channel)
        })
        .cloned()
        .collect()
}

/// Handle direct review-queue commands before routing to an agent.
/// `None` means the message is not a review command.
pub fn handle_review_command(project_root: &Path, message: &str) -> Option<String> {
    let normalized = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    let lower = normalized.to_lowercase();

    if matches!(
        lower.as_str(),
        "show workflow reviews" | "list workflow reviews" | "show review queue"
    ) {
        let queue = ReviewQueue::new(project_root);
        let items = queue.list_reviews("pending");
        if items.is_empty() {
            return Some("Workflow review queue is empty.".to_string());
        }
        let mut lines = vec!["Pending workflow reviews:".to_string()];
        for item in items {
            lines.push(format!(
                "- {}: {} — {}",
                item.review_id, item.workflow.name, item.workflow.description
            ));
        }
        lines.push(
            "Reply with \"approve workflow <review_id>\" or \"reject workflow <review_id>\"."
                .to_string(),
        );
        return Some(lines.join("\n"));
    }

    let approve_re = Regex::new(r"^approve workflow ([a-z0-9_-]+)$").expect("static regex");
    if let Some(captures) = approve_re.captures(&lower) {
        let review_id = captures.get(1).unwrap().as_str();
        let queue = ReviewQueue::new(project_root);
        return match queue.approve_workflow(review_id) {
            Ok(item) => {
                let mut registry = WorkflowRegistry::new(project_root);
                registry.reload(project_root);
                Some(format!(
                    "Approved workflow review {}. Published shared workflow: {}.",
                    item.review_id, item.workflow.name
                ))
            }
            Err(e) => Some(e),
        };
    }

    let reject_re =
        Regex::new(r"^reject workflow ([a-z0-9_-]+)(?:\s+(.+))?$").expect("static regex");
    if let Some(captures) = reject_re.captures(&lower) {
        let review_id = captures.get(1).unwrap().as_str();
        let notes = normalized["reject workflow ".len() + review_id.len()..]
            .trim()
            .to_string();
        let queue = ReviewQueue::new(project_root);
        return match queue.reject_workflow(review_id, &notes) {
            Ok(item) => Some(format!("Rejected workflow review {}.", item.review_id)),
            Err(e) => Some(e),
        };
    }

    None
}

/// The provider-prefixed model for a tier, from config (env overrides win).
pub fn select_model(project_root: &Path, tier: i64) -> String {
    let tiers = nerv_shared::config::NervConfig::load(project_root)
        .map(|c| c.models.tiers)
        .unwrap_or_default();
    let configured = match tier {
        1 => &tiers.tier1,
        2 => &tiers.tier2,
        3 => &tiers.tier3,
        _ => &tiers.tier0,
    };
    if configured.is_empty() {
        return tiers.tier0.clone();
    }
    configured.clone()
}

/// Whether the provider-prefixed `model` can serve a request right now
/// (port of `_model_available`).
pub async fn model_available(model: &str) -> bool {
    let (provider, name) = crate::llm::parse_model(model);
    match provider {
        // Uses the logged-in subscription, not an API key.
        "claude-cli" => {
            which_exists(&std::env::var("NERV_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string()))
        }
        "local" => {
            let url = std::env::var("NERV_OLLAMA_URL")
                .unwrap_or_else(|_| "http://localhost:11434".to_string());
            let client = reqwest::Client::new();
            matches!(
                client
                    .post(format!("{url}/api/show"))
                    .json(&serde_json::json!({"name": name}))
                    .timeout(std::time::Duration::from_secs(5))
                    .send()
                    .await,
                Ok(resp) if resp.status().is_success()
            )
        }
        // litellm: the API key depends on the vendor.
        _ => {
            let vendor = name.split('/').next().unwrap_or("").to_lowercase();
            let key_checks: &[(&str, &str)] = &[
                ("gemini", "GEMINI_API_KEY"),
                ("claude", "ANTHROPIC_API_KEY"),
                ("anthropic", "ANTHROPIC_API_KEY"),
                ("gpt", "OPENAI_API_KEY"),
                ("openai", "OPENAI_API_KEY"),
                ("o1", "OPENAI_API_KEY"),
                ("deepseek", "DEEPSEEK_API_KEY"),
            ];
            key_checks
                .iter()
                .find(|(prefix, _)| vendor.starts_with(prefix))
                .map(|(_, env_var)| std::env::var(env_var).is_ok_and(|v| !v.is_empty()))
                .unwrap_or(false)
        }
    }
}

fn which_exists(bin: &str) -> bool {
    std::process::Command::new("which")
        .arg(bin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Everything before the model call (port of `Orchestrator.prepare_turn`).
pub async fn prepare_turn(
    brain: &BrainClient,
    project_root: &Path,
    message: &str,
    route_result: &crate::router::RouteResult,
    channel: &str,
    sender: &str,
) -> Result<PreparedTurn, BrainError> {
    // confirm/cancel and review commands never reach a model call; a late
    // confirm is caught here too (one may have been queued by the MCP server
    // since the facade's check ran).
    if let Some(reply) =
        handle_pending_command(project_root, brain, message, sender, channel).await?
    {
        return Ok(PreparedTurn::Direct(reply));
    }
    if let Some(reply) = handle_review_command(project_root, message) {
        return Ok(PreparedTurn::Direct(reply));
    }

    // The router's direct reply needs no agent.
    if !route_result.reply.is_empty() {
        return Ok(PreparedTurn::Direct(route_result.reply.clone()));
    }

    // Find the matching agent, or manufacture one.
    let mut registry = AgentRegistry::load(project_root);
    let agent = match registry.find_by_type(&route_result.agent_type) {
        Some(agent) => agent.clone(),
        None => {
            info!(
                "Agent '{}' not found. Manufacturing via AgentFactory...",
                route_result.agent_type
            );
            let factory = AgentFactory::new(project_root);
            match factory
                .create_agent(&route_result.agent_type, message)
                .await
            {
                Some(new_agent) => {
                    registry.insert(new_agent.clone());
                    info!(
                        "Successfully birthed and registered new expert: '{}'",
                        new_agent.name
                    );
                    new_agent
                }
                None => {
                    tracing::warn!(
                        "AgentFactory failed for type '{}', using default general fallback",
                        route_result.agent_type
                    );
                    registry.get_default()
                }
            }
        }
    };
    info!(
        "Dispatching to agent '{}' (intent={}, tier={})",
        agent.name, route_result.intent, route_result.model_tier
    );

    let model = select_model(project_root, route_result.model_tier);

    // ── Fallback Workflow Gate ──
    // For tier >= 1: if the selected model is unavailable, attempt to fulfil
    // the request via a user-configured fallback workflow. The executor is
    // still python, so the gate hands over through one RPC.
    if route_result.model_tier >= 1 && !model_available(&model).await {
        if let Some(reply) = try_workflow_fallback(
            brain,
            project_root,
            &agent,
            message,
            route_result.model_tier,
            &model,
        )
        .await?
        {
            return Ok(PreparedTurn::Direct(reply));
        }
    }

    let tools_schemas = if agent.tools.is_empty() {
        None
    } else {
        let schemas = crate::tools::ToolRegistry.get_schemas(&agent.tools);
        if schemas.is_empty() {
            None
        } else {
            Some(schemas)
        }
    };

    let store = PendingActionStore::new(project_root);
    let known_pending = visible_pending_actions(&store.list(), sender, channel);

    let workflow_registry = WorkflowRegistry::new(project_root);
    let skill_registry = crate::skills::SkillRegistry::new(project_root);
    let catalogs = vec![
        workflow_registry.render_catalog(
            &agent.name,
            &agent.tags,
            agent.max_context_tokens as usize,
        ),
        skill_registry.render_catalog(&agent.name, &agent.tags, agent.max_context_tokens as usize),
    ];

    let system_prompt = crate::orchestrator::build_system_prompt(&agent);
    Ok(PreparedTurn::Agent {
        agent,
        system_prompt,
        model,
        catalogs,
        tools_schemas,
        known_pending,
    })
}

/// The fallback-workflow handover to the python executor. `None` means no
/// fallback is needed or available (drop to the tier-0 model as before).
#[allow(clippy::too_many_arguments)]
async fn try_workflow_fallback(
    brain: &BrainClient,
    _project_root: &Path,
    agent: &AgentDefinition,
    message: &str,
    tier: i64,
    model: &str,
) -> Result<Option<String>, BrainError> {
    let response = brain
        .call(
            "workflow_fallback",
            json!({
                "message": message,
                "tier": tier,
                "model": model,
                "agent": {
                    "name": agent.name,
                    "description": agent.description,
                    "tags": agent.tags,
                    "model_tier": agent.model_tier,
                    "system_prompt": agent.system_prompt,
                    "tools": agent.tools,
                    "max_context_tokens": agent.max_context_tokens,
                    "summary_strategy": agent.summary_strategy,
                    "created_by": agent.created_by,
                },
            }),
        )
        .await?;
    let reply = response["reply"].as_str().unwrap_or("");
    if reply.is_empty() {
        Ok(None)
    } else {
        Ok(Some(reply.to_string()))
    }
}

/// Finalize a turn: surface pendings queued during it that the reply does
/// not cover (port of `_append_new_pending`). Returns the possibly extended
/// reply and whether anything new was surfaced.
pub fn finish_turn(
    project_root: &Path,
    reply: &str,
    sender: &str,
    channel: &str,
    known_pending: &[PendingRecord],
) -> (String, bool) {
    let known_ids: HashSet<i64> = known_pending.iter().map(|a| a.id).collect();
    let store = PendingActionStore::new(project_root);
    let actions = visible_pending_actions(&store.list(), sender, channel);
    let new_actions: Vec<&PendingRecord> = actions
        .iter()
        .filter(|a| !known_ids.contains(&a.id) && !reply.contains(&format!("#{}", a.id)))
        .collect();
    if new_actions.is_empty() {
        return (reply.to_string(), false);
    }
    let mut lines = vec![String::new(), "Waiting for your confirmation:".to_string()];
    for action in new_actions {
        let reason = if action.reason.is_empty() {
            String::new()
        } else {
            format!(" — {}", action.reason)
        };
        lines.push(format!("- {}{reason}", format_pending_action(action)));
    }
    lines.push("Reply \"confirm <id>\" to run one, or \"cancel <id>\" to discard it.".to_string());
    (format!("{reply}\n{}", lines.join("\n")), true)
}
