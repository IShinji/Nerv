//! The brain entry point hosts drive: the transition-period facade that
//! combines migrated Rust modules with the Python stages that remain.
//!
//! The old brain sidecar answered one `route` method. Now the router runs in
//! Rust, so the flow splits into three steps (this is the migration pattern
//! every later module follows — see CLAUDE.md "Transition mechanics"):
//!
//! 1. `check_pending` (Python): `confirm <id>` / `cancel <id>` must never
//!    depend on a model call, so the pending queue is consulted first.
//! 2. The Rust router classifies locally — no round trip.
//! 3. `execute` (Python): the orchestrator dispatches with the routing
//!    decision. The response keeps the old `route` shape (the routing fields
//!    with `reply` overwritten), so hosts render it unchanged.

use std::path::Path;
use std::sync::Arc;

use nerv_shared::brain_client::{BrainClient, BrainError};
use serde_json::json;

use crate::router::{RouteResult, Router, RouterInitError};

pub struct BrainHandle {
    brain: Arc<BrainClient>,
    router: Router,
}

impl BrainHandle {
    /// Build a handle whose router resolves its model from config (env
    /// override wins) and loads the prompt data files from the project.
    pub fn new(brain: Arc<BrainClient>, project_root: &Path) -> Result<Self, RouterInitError> {
        Ok(Self {
            brain,
            router: Router::new(project_root)?,
        })
    }

    /// Build a handle from an explicit router — tests inject one whose
    /// backend fails fast (keyword fallback) so no model is needed.
    pub fn with_router(brain: Arc<BrainClient>, router: Router) -> Self {
        Self { brain, router }
    }

    /// Classify and dispatch one user message, returning the same JSON shape
    /// the old `route` method answered (routing fields + final `reply`).
    pub async fn reply(
        &self,
        message: &str,
        channel: &str,
        sender: &str,
    ) -> Result<serde_json::Value, BrainError> {
        // confirm/cancel <id> is handled before classification, so approving
        // a queued action never depends on a model call succeeding.
        let pending = self
            .brain
            .call(
                "check_pending",
                json!({"message": message, "channel": channel, "sender": sender}),
            )
            .await?;
        if pending.get("pending").and_then(|v| v.as_bool()) == Some(true) {
            let reply = pending.get("reply").and_then(|v| v.as_str()).unwrap_or("");
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

        // Dispatch to the orchestrator for the actual agent response.
        let response = self
            .brain
            .call(
                "execute",
                json!({
                    "message": message,
                    "channel": channel,
                    "sender": sender,
                    "route_result": serde_json::to_value(&route_result)
                        .expect("serializes"),
                }),
            )
            .await?;
        Ok(response)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    use nerv_shared::brain_client::BrainSpawnConfig;

    /// A fake brain speaking the check_pending/execute protocol. It records
    /// the methods it saw in `method_log` so tests can assert the call order.
    fn fake_brain(method_log: &Path) -> BrainSpawnConfig {
        let script = format!(
            r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":[[:space:]]*\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"ping"'*)
      printf '{{"jsonrpc":"2.0","result":{{"status":"ok"}},"id":%s}}\n' "$id" ;;

    *'"method":"check_pending"'*)
      printf '%s\n' "check_pending" >> "{log}"
      if printf '%s' "$line" | grep -q '"confirm 3"'; then
        printf '{{"jsonrpc":"2.0","result":{{"pending":true,"reply":"approved"}},"id":%s}}\n' "$id"
      else
        printf '{{"jsonrpc":"2.0","result":{{"pending":false,"reply":""}},"id":%s}}\n' "$id"
      fi ;;
    *'"method":"execute"'*)
      printf '%s\n' "execute" >> "{log}"
      printf '{{"jsonrpc":"2.0","result":{{"intent":"general","agent_type":"general","model_tier":0,"reply":"agent answer"}},"id":%s}}\n' "$id" ;;
    *'"method":"explode"'*)
      printf '{{"jsonrpc":"2.0","error":{{"code":-32000,"message":"boom"}},"id":%s}}\n' "$id" ;;
  esac
done
"#,
            log = method_log.display()
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

    /// A router whose backend fails immediately (unsupported provider), so
    /// classify falls back to keywords — deterministic, no model needed.
    fn fallback_router() -> Router {
        Router::with_parts("litellm:unused", "system", "template")
    }

    async fn started_handle(method_log: &Path) -> BrainHandle {
        let brain = Arc::new(BrainClient::new(fake_brain(method_log)));
        brain.start().await.expect("fake brain starts");
        BrainHandle::with_router(brain, fallback_router())
    }

    async fn read_log(method_log: &Path) -> Vec<String> {
        std::fs::read_to_string(method_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[tokio::test]
    async fn normal_message_runs_check_pending_then_execute() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("methods.log");
        let handle = started_handle(&log).await;

        let result = handle
            .reply("deploy the server", "telegram", "42")
            .await
            .expect("reply succeeds");

        assert_eq!(result["reply"], "agent answer");
        assert_eq!(read_log(&log).await, vec!["check_pending", "execute"]);
    }

    #[tokio::test]
    async fn pending_confirmation_never_reaches_the_router_or_execute() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("methods.log");
        let handle = started_handle(&log).await;

        let result = handle
            .reply("confirm 3", "telegram", "42")
            .await
            .expect("pending reply succeeds");

        assert_eq!(result["intent"], "pending_action");
        assert_eq!(result["reply"], "approved");
        assert_eq!(read_log(&log).await, vec!["check_pending"]);
    }

    #[tokio::test]
    async fn execute_receives_the_rust_routing_decision() {
        let dir = tempfile::tempdir().unwrap();
        // A capture brain: record the execute params so the test can inspect
        // the route_result the Rust router produced.
        let capture = dir.path().join("params.json");
        let brain = Arc::new(BrainClient::new(execute_capture_brain(&capture)));
        brain.start().await.expect("capture brain starts");
        let handle = BrainHandle::with_router(brain, fallback_router());

        let _ = handle
            .reply("help me debug this python function", "cli", "dev")
            .await;

        let request: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&capture).unwrap()).unwrap();
        let params = &request["params"];
        let route_result = &params["route_result"];
        assert_eq!(route_result["intent"], "code_generation");
        assert_eq!(route_result["agent_type"], "coder");
        assert_eq!(route_result["model_tier"], 1);
        assert_eq!(params["channel"], "cli");
        assert_eq!(params["sender"], "dev");
    }

    fn execute_capture_brain(capture: &Path) -> BrainSpawnConfig {
        let script = format!(
            r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":[[:space:]]*\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"ping"'*)
      printf '{{"jsonrpc":"2.0","result":{{"status":"ok"}},"id":%s}}\n' "$id" ;;

    *'"method":"check_pending"'*)
      printf '{{"jsonrpc":"2.0","result":{{"pending":false,"reply":""}},"id":%s}}\n' "$id" ;;
    *'"method":"execute"'*)
      printf '%s' "$line" > "{capture}"
      printf '{{"jsonrpc":"2.0","result":{{"reply":"done"}},"id":%s}}\n' "$id" ;;
  esac
done
"#,
            capture = capture.display()
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

    #[tokio::test]
    async fn a_failing_brain_surfaces_its_error() {
        let brain = Arc::new(BrainClient::new(erroring_brain()));
        brain.start().await.expect("brain starts");
        let handle = BrainHandle::with_router(brain, fallback_router());

        let err = handle
            .reply("hello", "cli", "u")
            .await
            .expect_err("check_pending errors");
        assert!(matches!(err, BrainError::Rpc(_)));
    }

    fn erroring_brain() -> BrainSpawnConfig {
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
}
