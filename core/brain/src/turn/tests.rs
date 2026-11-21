//! Tests for the turn flow: the tool loop, pending replies, and the trace
//! line. The turn runs fully in-process (no sidecar); the model caller is
//! scripted and the project root is a tempdir per test.

use super::*;
use crate::llm::CompletionResult;
use std::sync::Mutex;

fn final_result(content: &str) -> CompletionResult {
    CompletionResult {
        content: content.to_string(),
        tool_calls: Vec::new(),
        runs_own_tool_loop: false,
        provider: "fake".to_string(),
        duration_ms: None,
        cost_usd: None,
        usage: None,
    }
}

fn tool_calls_result(calls: &[(&str, serde_json::Value)]) -> CompletionResult {
    CompletionResult {
        content: String::new(),
        tool_calls: calls
            .iter()
            .map(|(name, args)| crate::llm::ToolCall {
                function: crate::llm::FunctionCall {
                    name: (*name).to_string(),
                    arguments: args.clone(),
                },
            })
            .collect(),
        runs_own_tool_loop: false,
        provider: "fake".to_string(),
        duration_ms: None,
        cost_usd: None,
        usage: None,
    }
}

/// The runner is built directly against a temp project root.
fn started_runner(dir: &Path) -> TurnRunner {
    TurnRunner::with_max_rounds(dir, 3)
}

fn route_result() -> RouteResult {
    RouteResult {
        intent: "general".to_string(),
        complexity: "low".to_string(),
        model_tier: 0,
        agent_type: "general".to_string(),
        needs_capabilities: Vec::new(),
        preferred_execution_mode: "auto".to_string(),
        reply: String::new(),
    }
}

#[tokio::test]
async fn agent_turn_runs_locally_and_returns_the_model_answer() {
    let dir = tempfile::tempdir().unwrap();
    // No agents on disk: prepare falls back to the minimal General agent.
    let runner = started_runner(dir.path());
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![
        final_result("agent answer"),
    ]));

    let (reply, _meta) = runner
        .run_turn("hello", &route_result(), "cli", "", caller.clone(), None)
        .await
        .expect("turn succeeds");

    // The reply is the model's answer — finalization is local, there is no
    // python round trip to rewrite it.
    assert_eq!(reply, "agent answer");
    // No leaf RPC at all: no run_tool, no notifications.
    // The turn was remembered.
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let conv = dir
        .path()
        .join("personal/memory/scopes/cli/conversations")
        .join(format!("{today}.jsonl"));
    let body = std::fs::read_to_string(conv).unwrap();
    assert!(body.contains("agent answer"), "{body}");
}

#[tokio::test]
async fn direct_reply_skips_the_model_caller() {
    let dir = tempfile::tempdir().unwrap();
    let runner = started_runner(dir.path());
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![]));

    let route = RouteResult {
        reply: "hi there".to_string(),
        ..route_result()
    };
    let (reply, _meta) = runner
        .run_turn("你好", &route, "cli", "", caller.clone(), None)
        .await
        .expect("turn succeeds");

    assert_eq!(reply, "hi there");
    assert!(caller.recorded_calls().is_empty());
}

#[tokio::test]
async fn suspended_tool_calls_queue_a_pending_and_notify() {
    let dir = tempfile::tempdir().unwrap();
    let runner = started_runner(dir.path());
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![
        tool_calls_result(&[("shell", serde_json::json!({"command": "rm -rf /"}))]),
    ]));

    let (reply, _meta) = runner
        .run_turn("rm it", &route_result(), "cli", "", caller.clone(), None)
        .await
        .expect("turn succeeds");

    // `shell` is native: the suspension happens in the rust registry, the
    // pending is queued in the shared store, and the reply carries the
    // new-pending surfacing (no run_tool RPC). The background alert reaches
    // the heartbeat drain through the rust notification queue.
    // The loop's suspension reply lists the queued action; the finalization
    // does not repeat it (the reply already names #1).
    assert!(
        reply.contains("I queued the following high-risk action(s)"),
        "{reply}"
    );
    assert!(reply.contains("rm -rf /"), "{reply}");
    let store = crate::tools::pending_store::PendingActionStore::new(dir.path());
    let queued = store.list();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].tool_name, "shell");
    // The background alert reaches the heartbeat drain through the rust
    // notification queue (drained and asserted by proactive's own tests).
}

#[tokio::test]
async fn executed_tool_results_feed_back_into_the_next_round() {
    let dir = tempfile::tempdir().unwrap();
    // `current_time` is native with a deterministic, harmless result.
    let runner = started_runner(dir.path());
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![
        tool_calls_result(&[("current_time", serde_json::json!({}))]),
        final_result("answer with tools"),
    ]));

    let (reply, _meta) = runner
        .run_turn("hello", &route_result(), "cli", "", caller.clone(), None)
        .await
        .expect("turn succeeds");

    assert_eq!(reply, "answer with tools");
    let calls = caller.recorded_calls();
    assert_eq!(calls.len(), 2);
    // Round two carries the assistant tool-call message and the tool result.
    let second = &calls[1];
    assert!(second
        .iter()
        .any(|m| m.role == "assistant" && m.tool_calls.is_some()));
    assert!(second
        .iter()
        .any(|m| m.role == "tool" && !m.content.is_empty()));
}

#[tokio::test]
async fn unknown_tools_report_back_to_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let runner = started_runner(dir.path());
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![
        tool_calls_result(&[("bogus_tool", serde_json::json!({}))]),
        final_result("recovered"),
    ]));

    let (reply, _meta) = runner
        .run_turn("hello", &route_result(), "cli", "", caller.clone(), None)
        .await
        .expect("turn succeeds");
    assert_eq!(reply, "recovered");
    let calls = caller.recorded_calls();
    assert!(calls[1]
        .iter()
        .any(|m| m.role == "tool" && m.content == "Unknown tool: bogus_tool"));
}

#[tokio::test]
async fn the_loop_stops_after_the_round_cap() {
    let dir = tempfile::tempdir().unwrap();
    let runner = started_runner(dir.path());
    // An unknown tool keeps the loop going without suspending (a registry
    // tool like `shell` would suspend on round one).
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(
        std::iter::repeat_with(|| {
            tool_calls_result(&[("bogus_round_tool", serde_json::json!({}))])
        })
        .take(5)
        .collect(),
    ));

    let (reply, _meta) = runner
        .run_turn("hello", &route_result(), "cli", "", caller.clone(), None)
        .await
        .expect("turn succeeds");
    assert!(
        reply.contains("I stopped after 3 rounds of tool calls"),
        "{reply}"
    );
    assert_eq!(caller.recorded_calls().len(), 3);
}

#[tokio::test]
async fn a_failing_model_writes_an_error_trace_line() {
    let dir = tempfile::tempdir().unwrap();
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
            _progress: Option<&crate::llm::ProgressCallback>,
        ) -> Result<crate::llm::CompletionResult, crate::llm::LlmError> {
            Err(crate::llm::LlmError::Unsupported("boom".into()))
        }
    }
    let runner = started_runner(dir.path());
    let caller = std::sync::Arc::new(AlwaysFail);

    // Loop failures degrade gracefully; the trace line records the error.
    let (reply, _meta) = runner
        .run_turn("hello", &route_result(), "cli", "", caller.clone(), None)
        .await
        .expect("turn degrades");

    assert!(
        reply.contains("I hit a problem reaching the model"),
        "{reply}"
    );
    let trace_path = dir.path().join("personal").join("logs").join("trace.jsonl");
    let line = std::fs::read_to_string(&trace_path).expect("trace written");
    let record: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(record["error"], "LlmError");
    assert_eq!(record["channel"], "cli");
    assert_eq!(record["scope"], "cli");
}

#[tokio::test]
async fn successful_turns_write_a_trace_line_with_the_routing_fields() {
    let dir = tempfile::tempdir().unwrap();
    let runner = started_runner(dir.path());
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![
        final_result("ok"),
    ]));

    runner
        .run_turn(
            "hello",
            &route_result(),
            "telegram",
            "42",
            caller.clone(),
            None,
        )
        .await
        .expect("turn succeeds");

    let trace_path = dir.path().join("personal").join("logs").join("trace.jsonl");
    let line = std::fs::read_to_string(&trace_path).expect("trace written");
    let record: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(record["channel"], "telegram");
    assert_eq!(record["scope"], "telegram-42");
    assert_eq!(record["tier"], 0);
    // The model comes from the local tier selection (config default).
    assert_eq!(record["model"], "claude-cli:opus");
    assert_eq!(record["provider"], "fake");
    assert_eq!(record["error"], serde_json::Value::Null);
    assert!(record["duration_ms"].as_f64().unwrap() >= 0.0);
}

// --- pending commands (confirm / cancel), now matched locally ---

#[tokio::test]
async fn a_late_confirm_is_caught_by_the_prepare_stage() {
    let dir = tempfile::tempdir().unwrap();
    // The MCP server (a separate process) queued an action before the turn.
    let store = crate::tools::pending_store::PendingActionStore::new(dir.path());
    store
        .add(
            "current_time",
            serde_json::json!({}),
            "for the test",
            "",
            "cli",
            "",
            "",
        )
        .unwrap();
    let runner = started_runner(dir.path());
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![]));

    let (reply, _meta) = runner
        .run_turn(
            "confirm 1",
            &route_result(),
            "cli",
            "",
            caller.clone(),
            None,
        )
        .await
        .expect("turn succeeds");

    assert!(reply.contains("Executed pending action #1"), "{reply}");
    assert!(reply.contains("current_time"), "{reply}");
    // No model call, no leaf RPC.
    assert!(caller.recorded_calls().is_empty());
    assert!(store.list().is_empty());
}

#[tokio::test]
async fn casual_words_never_touch_queued_actions() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::tools::pending_store::PendingActionStore::new(dir.path());
    store
        .add(
            "current_time",
            serde_json::json!({}),
            "for the test",
            "",
            "cli",
            "",
            "",
        )
        .unwrap();
    let runner = started_runner(dir.path());
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![
        final_result("yes, sure"),
    ]));

    // A bare "yes" is chat, not a confirmation.
    let (reply, _meta) = runner
        .run_turn("yes", &route_result(), "cli", "", caller.clone(), None)
        .await
        .expect("turn succeeds");
    assert_eq!(reply, "yes, sure");
    assert_eq!(store.list().len(), 1);
}

#[tokio::test]
async fn cancel_discards_a_queued_action() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::tools::pending_store::PendingActionStore::new(dir.path());
    store
        .add(
            "shell",
            serde_json::json!({"command": "rm it"}),
            "risky",
            "",
            "cli",
            "",
            "",
        )
        .unwrap();
    let runner = started_runner(dir.path());
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![]));

    let (reply, _meta) = runner
        .run_turn("cancel 1", &route_result(), "cli", "", caller.clone(), None)
        .await
        .expect("turn succeeds");

    assert!(reply.contains("Cancelled pending action #1"), "{reply}");
    assert!(store.list().is_empty());
    assert!(caller.recorded_calls().is_empty());
}

#[test]
fn scope_for_slugs_channel_and_sender() {
    assert_eq!(scope_for("", ""), "default");
    assert_eq!(scope_for("telegram", "42"), "telegram-42");
    assert_eq!(scope_for("Telegram", "User@1"), "telegram-user-1");
    assert_eq!(scope_for("  ", ""), "default");
}

#[tokio::test]
async fn tool_execution_pushes_status_to_the_progress_feed() {
    let dir = tempfile::tempdir().unwrap();
    let runner = started_runner(dir.path());
    // `current_time` is native and harmless; run through the agent path.
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![
        tool_calls_result(&[("current_time", serde_json::json!({}))]),
        final_result("done"),
    ]));

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let spy = Arc::clone(&seen);
    let progress = move |snapshot: &str| {
        spy.lock().unwrap().push(snapshot.to_string());
    };

    runner
        .run_turn(
            "hello",
            &route_result(),
            "cli",
            "",
            caller.clone(),
            Some(&progress),
        )
        .await
        .expect("turn succeeds");

    let seen = seen.lock().unwrap();
    assert!(
        seen.iter().any(|s| s.contains("⚙ current_time")),
        "tool status must reach the progress feed: {seen:?}"
    );
}
