//! The brain entry point hosts drive: the fully-Rust brain facade.
//!
//! Everything runs in-process — pending commands, the router, the turn loop,
//! the tool registry, memory, workflows and the litellm cloud backends. The
//! python sidecar is retired; this handle is the single host entry (the
//! gateway and the GUI both call it).
//!
//! Responses keep the old `route` shape (routing fields + final `reply`), so
//! hosts render them unchanged.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nerv_shared::brain_client::BrainError;
use serde_json::json;

use crate::llm::claude_cli::ClaudeCliBackend;
use crate::router::{RouteResult, Router, RouterInitError};
use crate::turn::{ModelCaller, ProviderCaller, TraceRecord, TurnRunner};

pub struct BrainHandle {
    router: Router,
    caller: Arc<dyn ModelCaller>,
    project_root: PathBuf,
}

impl BrainHandle {
    /// Build a handle whose router resolves its model from config (env
    /// override wins) and loads the prompt data files from the project.
    pub fn new(project_root: &Path) -> Result<Self, RouterInitError> {
        let router = Router::new(project_root)?;
        let caller = Arc::new(ProviderCaller {
            claude_cli_backend: ClaudeCliBackend::default(),
            project_root: project_root.to_path_buf(),
        });
        Ok(Self {
            router,
            caller,
            project_root: project_root.to_path_buf(),
        })
    }

    /// Build a handle from explicit parts — tests inject a fallback router
    /// (no model needed) and a scripted model caller.
    pub fn with_parts(router: Router, caller: Arc<dyn ModelCaller>, project_root: &Path) -> Self {
        Self {
            router,
            caller,
            project_root: project_root.to_path_buf(),
        }
    }

    /// Classify and run one user message, returning the same JSON shape the
    /// old `route` method answered (routing fields + final `reply`).
    pub async fn reply(
        &self,
        message: &str,
        channel: &str,
        sender: &str,
    ) -> Result<serde_json::Value, BrainError> {
        // confirm/cancel <id> is handled before classification, so approving
        // a queued action never depends on a model call succeeding. The
        // command matching runs locally (turn::prepare).
        let scope = crate::turn::scope_for(channel, sender);
        let pending = crate::turn::prepare::handle_pending_command(
            &self.project_root,
            message,
            sender,
            channel,
        )
        .await?;
        if let Some(reply) = pending {
            TraceRecord::new(channel, &scope).write(&self.project_root);
            let result = RouteResult {
                intent: "pending_action".to_string(),
                complexity: "low".to_string(),
                model_tier: 0,
                agent_type: "general".to_string(),
                needs_capabilities: Vec::new(),
                preferred_execution_mode: "auto".to_string(),
                reply: reply.to_string(),
            };
            return Ok(serde_json::to_value(&result).expect("serializes"));
        }

        let route_result = self.router.classify(message).await;

        let runner = TurnRunner::new(&self.project_root);
        let reply = runner
            .run_turn(message, &route_result, channel, sender, self.caller.clone())
            .await?;

        let mut result = serde_json::to_value(&route_result).expect("serializes");
        result["reply"] = json!(reply);
        Ok(result)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::llm::CompletionResult;
    use crate::turn::model_loop::testing::FakeModelCaller;

    fn fallback_router() -> Router {
        Router::with_parts("litellm:unused", "system", "template")
    }

    fn scripted_caller(content: &str) -> Arc<dyn ModelCaller> {
        Arc::new(FakeModelCaller::new(vec![CompletionResult {
            content: content.to_string(),
            tool_calls: Vec::new(),
            runs_own_tool_loop: false,
            provider: String::new(),
            duration_ms: None,
            cost_usd: None,
            usage: None,
        }]))
    }

    #[tokio::test]
    async fn normal_message_runs_the_local_turn() {
        let dir = tempfile::tempdir().unwrap();
        let handle = BrainHandle::with_parts(
            fallback_router(),
            scripted_caller("agent answer"),
            dir.path(),
        );

        // "hello" routes at tier 0, so the fallback-workflow gate (which
        // needs a claude binary on PATH) is never consulted.
        let result = handle
            .reply("hello", "cli", "dev")
            .await
            .expect("reply succeeds");

        assert_eq!(result["reply"], "agent answer");
    }

    #[tokio::test]
    async fn pending_confirmation_runs_without_any_model_call() {
        let dir = tempfile::tempdir().unwrap();
        // The MCP server (a separate process) queued an action earlier.
        let store = crate::tools::pending_store::PendingActionStore::new(dir.path());
        store
            .add(
                "current_time",
                serde_json::json!({}),
                "for the test",
                "dev",
                "cli",
                "",
                "",
            )
            .unwrap();
        let handle = BrainHandle::with_parts(
            fallback_router(),
            scripted_caller("must not be called"),
            dir.path(),
        );

        let result = handle
            .reply("confirm 1", "cli", "dev")
            .await
            .expect("pending reply succeeds");

        assert_eq!(result["intent"], "pending_action");
        assert!(
            result["reply"]
                .as_str()
                .unwrap()
                .contains("Executed pending action #1"),
            "{}",
            result["reply"]
        );
        assert!(store.list().is_empty());
    }

    #[tokio::test]
    async fn the_fallback_gate_executes_a_browser_workflow_natively() {
        let dir = tempfile::tempdir().unwrap();
        // The config loader always reads core/config.default.yaml from the
        // project root first, so stage a minimal one.
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("config.default.yaml"),
            dir.path().join("core/config.default.yaml"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("config.yaml"),
            "models:\n  tiers:\n    tier1: \"litellm:gpt-4o\"\n",
        )
        .unwrap();
        // The loader refuses world-readable files, so keep it owner-only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.path().join("config.yaml"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }

        // A user-configured browser-tagged workflow serves as the fallback.
        std::fs::create_dir_all(dir.path().join("personal/workflows")).unwrap();
        std::fs::write(
            dir.path().join("personal/workflows/fallback.yaml"),
            "name: FallbackDemo\ndescription: d\ntags: [browser]\nsteps:\n  - \"Summarize the request\"\n",
        )
        .unwrap();

        // No API key for the litellm model: the gate runs the workflow with
        // the native executor instead of dropping to tier 0.
        std::env::remove_var("OPENAI_API_KEY");
        let handle =
            BrainHandle::with_parts(fallback_router(), scripted_caller("unused"), dir.path());

        // A code-generation route is tier 1 — the gate applies.
        let result = handle
            .reply("help me debug this python function", "cli", "u")
            .await
            .expect("the fallback run completes");
        // The executor ran the workflow: its reply names the workflow or the
        // failed step (a reasoning step needs the real backend).
        let reply = result["reply"].as_str().unwrap_or("");
        assert!(
            reply.contains("Step 1 Result") || reply.contains("failed at step 1"),
            "{reply}"
        );
    }
}
