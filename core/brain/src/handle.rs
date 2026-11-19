//! The brain entry point hosts drive: the transition-period facade that
//! combines migrated Rust modules with the Python stages that remain.
//!
//! The old brain sidecar answered one `route` method. Now the router and the
//! turn loop run in Rust (see CLAUDE.md "Transition mechanics"); python keeps
//! the leaf operations that have not migrated behind thin RPCs:
//!
//! 1. `check_pending` (python): `confirm <id>` / `cancel <id>` must never
//!    depend on a model call, so the pending queue is consulted first.
//! 2. The Rust router classifies locally — no round trip.
//! 3. `core/brain/src/turn` runs the agent turn: context/agent selection via
//!    `prepare_turn`, the model loop (rust llm), tool execution via
//!    `run_tool`, finalization via `finish_turn`.
//!
//! Responses keep the old `route` shape (routing fields + final `reply`), so
//! hosts render them unchanged.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nerv_shared::brain_client::{BrainClient, BrainError};
use serde_json::json;

use crate::llm::claude_cli::ClaudeCliBackend;
use crate::router::{RouteResult, Router, RouterInitError};
use crate::turn::{ModelCaller, ProviderCaller, TraceRecord, TurnRunner};

pub struct BrainHandle {
    brain: Arc<BrainClient>,
    router: Router,
    caller: Arc<dyn ModelCaller>,
    project_root: PathBuf,
}

impl BrainHandle {
    /// Build a handle whose router resolves its model from config (env
    /// override wins) and loads the prompt data files from the project.
    pub fn new(brain: Arc<BrainClient>, project_root: &Path) -> Result<Self, RouterInitError> {
        let router = Router::new(project_root)?;
        let caller = Arc::new(ProviderCaller {
            claude_cli_backend: ClaudeCliBackend::default(),
            project_root: project_root.to_path_buf(),
            brain: brain.clone(),
        });
        Ok(Self {
            brain,
            router,
            caller,
            project_root: project_root.to_path_buf(),
        })
    }

    /// Build a handle from explicit parts — tests inject a fallback router
    /// (no model needed) and a scripted model caller.
    pub fn with_parts(
        brain: Arc<BrainClient>,
        router: Router,
        caller: Arc<dyn ModelCaller>,
        project_root: &Path,
    ) -> Self {
        Self {
            brain,
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
        // command matching runs locally (turn::prepare); python is only
        // reached when a confirmed python-only tool executes.
        let scope = crate::turn::scope_for(channel, sender);
        let pending = crate::turn::prepare::handle_pending_command(
            &self.project_root,
            &self.brain,
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

        let runner = TurnRunner::new(self.brain.clone(), &self.project_root);
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
    use nerv_shared::brain_client::BrainSpawnConfig;
    use std::time::Duration;

    fn fallback_router() -> Router {
        Router::with_parts("litellm:unused", "system", "template")
    }

    /// A fake brain implementing the leaf protocol that is still python:
    /// `run_tool` errors unless the test scripts a response; everything the
    /// turn does besides leaf calls is local now.
    fn fake_brain() -> BrainSpawnConfig {
        let script = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":[[:space:]]*\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"ping"'*)
      printf '{"jsonrpc":"2.0","result":{"status":"ok"},"id":%s}\n' "$id" ;;
    *'"method":"queue_notifications"'*)
      printf '{"jsonrpc":"2.0","result":{"ok":true},"id":%s}\n' "$id" ;;
    *'"method":"execute_pending"'*)
      printf '{"jsonrpc":"2.0","result":{"content":"python ran it"},"id":%s}\n' "$id" ;;
    *)
      printf '{"jsonrpc":"2.0","error":{"code":-32000,"message":"boom"},"id":%s}\n' "$id" ;;
  esac
done
"#;
        BrainSpawnConfig {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            working_dir: std::env::temp_dir(),
            env: Vec::new(),
            restart_max: 0,
            restart_window: Duration::from_secs(600),
            restart_backoff: Duration::from_millis(10),
            call_timeout: Duration::from_secs(5),
        }
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
        let brain = Arc::new(BrainClient::new(fake_brain()));
        brain.start().await.expect("fake brain starts");
        let handle = BrainHandle::with_parts(
            brain,
            fallback_router(),
            scripted_caller("agent answer"),
            dir.path(),
        );

        // "hello" routes at tier 0, so the fallback-workflow gate (which
        // needs a claude binary on PATH) is never consulted in CI.
        let result = handle
            .reply("hello", "cli", "dev")
            .await
            .expect("reply succeeds");

        assert_eq!(result["reply"], "agent answer");
    }

    #[tokio::test]
    async fn pending_confirmation_runs_without_any_rpc() {
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
        let brain = Arc::new(BrainClient::new(fake_brain()));
        brain.start().await.expect("fake brain starts");
        let handle = BrainHandle::with_parts(
            brain,
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
        let cfg = dir.path().join("config.yaml");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::env::set_var("NERV_CONFIG", &cfg);

        // A user-configured browser-tagged workflow serves as the fallback.
        std::fs::create_dir_all(dir.path().join("personal/workflows")).unwrap();
        std::fs::write(
            dir.path().join("personal/workflows/fallback.yaml"),
            "name: FallbackDemo\ndescription: d\ntags: [browser]\nsteps:\n  - \"web_search nervous systems\"\n",
        )
        .unwrap();

        let brain = Arc::new(BrainClient::new(fake_brain()));
        brain.start().await.expect("brain starts");
        let handle = BrainHandle::with_parts(
            brain,
            fallback_router(),
            scripted_caller("unused"),
            dir.path(),
        );

        // The litellm model has no API key: the gate runs the workflow with
        // the native executor instead of dropping to tier 0. The reply comes
        // from the workflow run, not the model caller.
        let result = handle
            .reply("help me debug this python function", "cli", "u")
            .await
            .expect("the fallback run completes");
        // The executor ran the workflow: its reply names the workflow (a
        // reasoning step needs the real backend, so offline CI reports the
        // step failure — either way the run happened, not a model call).
        let reply = result["reply"].as_str().unwrap_or("");
        assert!(
            reply.contains("Step 1 Result") || reply.contains("failed at step 1"),
            "{reply}"
        );
    }
}
