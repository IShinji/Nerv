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
        // facade owns the trace line (python's request_trace module is gone).
        let scope = crate::turn::scope_for(channel, sender);
        let pending = self
            .brain
            .call(
                "check_pending",
                json!({"message": message, "channel": channel, "sender": sender}),
            )
            .await?;
        if pending.get("pending").and_then(|v| v.as_bool()) == Some(true) {
            let reply = pending.get("reply").and_then(|v| v.as_str()).unwrap_or("");
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

    /// A fake brain implementing the check_pending/prepare_turn/finish_turn
    /// protocol; method names are logged in call order, prepare_turn params
    /// captured for assertions.
    fn fake_brain(method_log: &Path) -> BrainSpawnConfig {
        let capture = method_log.parent().unwrap().join("prepare_capture.json");
        let script = format!(
            r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":[[:space:]]*\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"ping"'*)
      printf '{{"jsonrpc":"2.0","result":{{"status":"ok"}},"id":%s}}\n' "$id" ;;
    *'"method":"check_pending"'*)
      printf '%s\n' "check_pending" >> "{log}"
      printf '{{"jsonrpc":"2.0","result":{{"pending":false,"reply":""}},"id":%s}}\n' "$id" ;;
    *'"method":"prepare_turn"'*)
      printf '%s\n' "prepare_turn" >> "{log}"
      printf '%s' "$line" > "{capture}"
      printf '{{"jsonrpc":"2.0","result":{{"status":"agent","agent":{{"name":"Coder","tools":[]}},"model":"claude-cli:fake","messages":[{{"role":"user","content":"x"}}],"tools_schemas":null,"known_pending":[]}},"id":%s}}\n' "$id" ;;
    *'"method":"finish_turn"'*)
      printf '%s\n' "finish_turn" >> "{log}"
      printf '{{"jsonrpc":"2.0","result":{{"reply":"finished","pending_created":false}},"id":%s}}\n' "$id" ;;
    *'"method":"run_tool"'*)
      printf '{{"jsonrpc":"2.0","error":{{"code":-32000,"message":"unexpected run_tool"}},"id":%s}}\n' "$id" ;;
  esac
done
"#,
            log = method_log.display(),
            capture = capture.display(),
        );
        BrainSpawnConfig {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script],
            working_dir: std::env::temp_dir(),
            env: Vec::new(),
            restart_max: 0,
            restart_window: Duration::from_secs(600),
            restart_backoff: Duration::from_millis(10),
            call_timeout: Duration::from_secs(5),
        }
    }

    fn scripted_caller() -> Arc<dyn ModelCaller> {
        Arc::new(FakeModelCaller::new(vec![CompletionResult {
            content: "agent answer".to_string(),
            tool_calls: Vec::new(),
            runs_own_tool_loop: false,
            provider: String::new(),
            duration_ms: None,
            cost_usd: None,
            usage: None,
        }]))
    }

    async fn read_log(method_log: &Path) -> Vec<String> {
        std::fs::read_to_string(method_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[tokio::test]
    async fn normal_message_runs_check_pending_prepare_and_finish() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("methods.log");
        let brain = Arc::new(BrainClient::new(fake_brain(&log)));
        brain.start().await.expect("fake brain starts");
        let handle =
            BrainHandle::with_parts(brain, fallback_router(), scripted_caller(), dir.path());

        let result = handle
            .reply("help me debug this python function", "cli", "dev")
            .await
            .expect("reply succeeds");

        assert_eq!(result["reply"], "finished");
        assert_eq!(
            read_log(&log).await,
            vec!["check_pending", "prepare_turn", "finish_turn"]
        );
    }

    #[tokio::test]
    async fn prepare_turn_receives_the_rust_routing_decision() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("methods.log");
        let brain = Arc::new(BrainClient::new(fake_brain(&log)));
        brain.start().await.expect("fake brain starts");
        let handle =
            BrainHandle::with_parts(brain, fallback_router(), scripted_caller(), dir.path());

        handle
            .reply("help me debug this python function", "cli", "dev")
            .await
            .expect("reply succeeds");

        let request: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("prepare_capture.json")).unwrap(),
        )
        .unwrap();
        let route_result = &request["params"]["route_result"];
        assert_eq!(route_result["intent"], "code_generation");
        assert_eq!(route_result["agent_type"], "coder");
        assert_eq!(route_result["model_tier"], 1);
        assert_eq!(request["params"]["channel"], "cli");
        assert_eq!(request["params"]["sender"], "dev");
    }

    #[tokio::test]
    async fn pending_confirmation_short_circuits_before_prepare() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("methods.log");
        let script = format!(
            r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":[[:space:]]*\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"ping"'*)
      printf '{{"jsonrpc":"2.0","result":{{"status":"ok"}},"id":%s}}\n' "$id" ;;
    *'"method":"check_pending"'*)
      printf '%s\n' "check_pending" >> "{log}"
      printf '{{"jsonrpc":"2.0","result":{{"pending":true,"reply":"approved"}},"id":%s}}\n' "$id" ;;
    *'"method":"prepare_turn"'*)
      printf '{{"jsonrpc":"2.0","error":{{"code":-32000,"message":"must not run"}},"id":%s}}\n' "$id" ;;
  esac
done
"#,
            log = log.display(),
        );
        let brain = Arc::new(BrainClient::new(BrainSpawnConfig {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script],
            working_dir: std::env::temp_dir(),
            env: Vec::new(),
            restart_max: 0,
            restart_window: Duration::from_secs(600),
            restart_backoff: Duration::from_millis(10),
            call_timeout: Duration::from_secs(5),
        }));
        brain.start().await.expect("brain starts");
        let handle =
            BrainHandle::with_parts(brain, fallback_router(), scripted_caller(), dir.path());

        let result = handle
            .reply("confirm 3", "cli", "dev")
            .await
            .expect("pending reply succeeds");

        assert_eq!(result["intent"], "pending_action");
        assert_eq!(result["reply"], "approved");
        assert_eq!(read_log(&log).await, vec!["check_pending"]);
    }

    #[tokio::test]
    async fn a_failing_brain_surfaces_its_error() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":[[:space:]]*\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"ping"'*)
      printf '{"jsonrpc":"2.0","result":{"status":"ok"},"id":%s}\n' "$id" ;;
    *)
      printf '{"jsonrpc":"2.0","error":{"code":-32000,"message":"boom"},"id":%s}\n' "$id" ;;
  esac
done
"#;
        let brain = Arc::new(BrainClient::new(BrainSpawnConfig {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            working_dir: std::env::temp_dir(),
            env: Vec::new(),
            restart_max: 0,
            restart_window: Duration::from_secs(600),
            restart_backoff: Duration::from_millis(10),
            call_timeout: Duration::from_secs(5),
        }));
        brain.start().await.expect("brain starts");
        let handle =
            BrainHandle::with_parts(brain, fallback_router(), scripted_caller(), dir.path());

        let err = handle
            .reply("hello", "cli", "u")
            .await
            .expect_err("check_pending errors");
        assert!(err.to_string().contains("boom"), "{err}");
    }
}
