//! Tests for the turn flow: protocol ordering, the tool loop, pending
//! replies, and the trace line. Fake brains are `/bin/sh` JSON-RPC scripts
//! (the brain_client test technique); the model caller is scripted.

use super::*;
use crate::llm::CompletionResult;
use nerv_shared::brain_client::BrainSpawnConfig;
use std::time::Duration;

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

/// A fake brain implementing the prepare_turn/run_tool/finish_turn protocol.
/// Method names are appended to `methods.log` in call order; run_tool
/// responses rotate through the provided list (the last repeats).
fn fake_brain(
    dir: &Path,
    prepare: serde_json::Value,
    run_tool_responses: &[serde_json::Value],
) -> BrainSpawnConfig {
    let prepare_file = dir.join("prepare.json");
    std::fs::write(&prepare_file, serde_json::to_string(&prepare).unwrap()).unwrap();
    let run_tool_file = dir.join("run_tool.jsonl");
    let run_tool_body: String = if run_tool_responses.is_empty() {
        serde_json::to_string(&serde_json::json!({"content": "", "suspended": false})).unwrap()
    } else {
        run_tool_responses
            .iter()
            .map(|v| serde_json::to_string(v).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    } + "\n";
    std::fs::write(&run_tool_file, run_tool_body).unwrap();
    let methods = dir.join("methods.log");

    let script = format!(
        r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":[[:space:]]*\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"ping"'*)
      printf '{{"jsonrpc":"2.0","result":{{"status":"ok"}},"id":%s}}\n' "$id" ;;
    *'"method":"prepare_turn"'*)
      printf '%s\n' "prepare_turn" >> "{methods}"
      printf '{{"jsonrpc":"2.0","result":%s,"id":%s}}\n' "$(cat {prepare_file})" "$id" ;;
    *'"method":"run_tool"'*)
      printf '%s\n' "run_tool" >> "{methods}"
      n=$(grep -c "run_tool" {methods} | tr -d ' ')
      total=$(wc -l < {run_tool_file} | tr -d ' ')
      if [ "$n" -gt "$total" ]; then n=$total; fi
      printf '{{"jsonrpc":"2.0","result":%s,"id":%s}}\n' "$(sed -n "${{n}}p" {run_tool_file})" "$id" ;;
    *'"method":"finish_turn"'*)
      printf '%s\n' "finish_turn" >> "{methods}"
      printf '%s' "$line" > "{finish_capture}"
      printf '{{"jsonrpc":"2.0","result":{{"reply":"finished","pending_created":false}},"id":%s}}\n' "$id" ;;
    *'"method":"complete_turn"'*)
      printf '{{"jsonrpc":"2.0","error":{{"code":-32000,"message":"unexpected complete_turn"}},"id":%s}}\n' "$id" ;;
  esac
done
"#,
        methods = methods.display(),
        prepare_file = prepare_file.display(),
        run_tool_file = run_tool_file.display(),
        finish_capture = dir.join("finish_capture.json").display(),
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

fn agent_prepare() -> serde_json::Value {
    serde_json::json!({
        "status": "agent",
        "agent": {"name": "General", "tools": []},
        "model": "claude-cli:fake",
        "messages": [
            {"role": "system", "content": "You are Nerv."},
            {"role": "user", "content": "hello"}
        ],
        "tools_schemas": null,
        "known_pending": []
    })
}

async fn started_runner(
    dir: &Path,
    prepare: serde_json::Value,
    run_tool_responses: &[serde_json::Value],
) -> (TurnRunner, PathBuf) {
    let brain = Arc::new(BrainClient::new(fake_brain(
        dir,
        prepare,
        run_tool_responses,
    )));
    brain.start().await.expect("fake brain starts");
    let runner = TurnRunner::with_max_rounds(brain, dir, 3);
    (runner, dir.join("methods.log"))
}

async fn read_log(methods: &Path) -> Vec<String> {
    std::fs::read_to_string(methods)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
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
async fn agent_turn_runs_prepare_then_finish_around_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, methods) = started_runner(dir.path(), agent_prepare(), &[]).await;
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![
        final_result("agent answer"),
    ]));

    let reply = runner
        .run_turn("hello", &route_result(), "cli", "", caller.clone())
        .await
        .expect("turn succeeds");

    assert_eq!(reply, "finished");
    assert_eq!(
        read_log(&methods).await,
        vec!["prepare_turn", "finish_turn"]
    );
}

#[tokio::test]
async fn direct_reply_skips_the_model_caller() {
    let dir = tempfile::tempdir().unwrap();
    let prepare = serde_json::json!({"status": "direct", "reply": "hi there"});
    let (runner, methods) = started_runner(dir.path(), prepare, &[]).await;
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![]));

    let reply = runner
        .run_turn("你好", &route_result(), "cli", "", caller.clone())
        .await
        .expect("turn succeeds");

    assert_eq!(reply, "finished");
    assert_eq!(
        read_log(&methods).await,
        vec!["prepare_turn", "finish_turn"]
    );
    assert!(caller.recorded_calls().is_empty());
}

#[tokio::test]
async fn suspended_tool_calls_end_the_round_with_a_pending_reply() {
    let dir = tempfile::tempdir().unwrap();
    let prepare = serde_json::json!({
        "status": "agent",
        "agent": {"name": "SysAdmin", "tools": ["shell"]},
        "model": "claude-cli:fake",
        "messages": [{"role": "user", "content": "rm it"}],
        "tools_schemas": [{"type": "function"}],
        "known_pending": []
    });
    let run_tool = serde_json::json!({
        "content": "[ACTION SUSPENDED] Pending action #7: shell with arguments {}",
        "suspended": true,
        "pending": {"id": 7, "tool_name": "shell", "arguments": {}, "reason": "matches the guard"},
        "notification": "⚠️ [SYSTEM BACKGROUND ALERT] Pending action #7 (shell) is waiting for confirmation."
    });
    let (runner, methods) = started_runner(dir.path(), prepare, &[run_tool]).await;
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![
        tool_calls_result(&[("shell", serde_json::json!({"command": "rm -rf /"}))]),
    ]));

    let reply = runner
        .run_turn("rm it", &route_result(), "cli", "", caller.clone())
        .await
        .expect("turn succeeds");

    assert_eq!(reply, "finished");
    let captured = std::fs::read_to_string(dir.path().join("finish_capture.json")).unwrap();
    assert!(
        captured.contains("I queued the following high-risk action(s) for confirmation:"),
        "{captured}"
    );
    assert!(
        // The capture file holds the raw json line (quotes escaped) — and
        // `shell` is queued natively (rust registry), so the fake brain's
        // run_tool response is never consulted.
        captured.contains("Pending action #1: shell with arguments")
            && captured.contains("rm -rf /"),
        "{captured}"
    );
    assert!(
        captured.contains("\\nReply with \\\"confirm <id>\\\""),
        "{captured}"
    );
    assert!(
        captured.contains("ALERT"),
        "notifications must reach finish_turn: {captured}"
    );
    // `shell` is native: no run_tool RPC — the suspension happens in rust.
    assert_eq!(
        read_log(&methods).await,
        vec!["prepare_turn", "finish_turn"]
    );
}

#[tokio::test]
async fn executed_tool_results_feed_back_into_the_next_round() {
    let dir = tempfile::tempdir().unwrap();
    let prepare = agent_prepare();
    let run_tool = serde_json::json!({"content": "tool output", "suspended": false});
    let (runner, _methods) = started_runner(dir.path(), prepare, &[run_tool]).await;
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![
        tool_calls_result(&[("web_search", serde_json::json!({"query": "x"}))]),
        final_result("answer with tools"),
    ]));

    let reply = runner
        .run_turn("hello", &route_result(), "cli", "", caller.clone())
        .await
        .expect("turn succeeds");

    assert_eq!(reply, "finished");
    let calls = caller.recorded_calls();
    assert_eq!(calls.len(), 2);
    // Round two carries the assistant tool-call message and the tool result.
    let second = &calls[1];
    assert!(second
        .iter()
        .any(|m| m.role == "assistant" && m.tool_calls.is_some()));
    assert!(second
        .iter()
        .any(|m| m.role == "tool" && m.content == "tool output"));
}

#[tokio::test]
async fn unknown_tools_report_back_to_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let prepare = agent_prepare();
    let run_tool = serde_json::json!({"content": "Unknown tool: bogus_tool", "suspended": false});
    let (runner, _methods) = started_runner(dir.path(), prepare, &[run_tool]).await;
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![
        tool_calls_result(&[("bogus_tool", serde_json::json!({}))]),
        final_result("recovered"),
    ]));

    let reply = runner
        .run_turn("hello", &route_result(), "cli", "", caller.clone())
        .await
        .expect("turn succeeds");
    assert_eq!(reply, "finished");
    let calls = caller.recorded_calls();
    assert!(calls[1]
        .iter()
        .any(|m| m.role == "tool" && m.content == "Unknown tool: bogus_tool"));
}

#[tokio::test]
async fn the_loop_stops_after_the_round_cap() {
    let dir = tempfile::tempdir().unwrap();
    let prepare = agent_prepare();
    let run_tool = serde_json::json!({"content": "tool output", "suspended": false});
    let (runner, _methods) = started_runner(dir.path(), prepare, &[run_tool]).await;
    // Use an unknown tool so every round goes through the python run_tool
    // RPC (a registry tool like `shell` would suspend on round one).
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(
        std::iter::repeat_with(|| {
            tool_calls_result(&[("bogus_round_tool", serde_json::json!({}))])
        })
        .take(5)
        .collect(),
    ));

    let reply = runner
        .run_turn("hello", &route_result(), "cli", "", caller.clone())
        .await
        .expect("turn succeeds");
    assert_eq!(reply, "finished");
    let captured = std::fs::read_to_string(dir.path().join("finish_capture.json")).unwrap();
    assert!(
        captured.contains("I stopped after 3 rounds of tool calls"),
        "{captured}"
    );
    assert_eq!(caller.recorded_calls().len(), 3);
}

#[tokio::test]
async fn a_failing_prepare_writes_an_error_trace_line() {
    let dir = tempfile::tempdir().unwrap();
    // A brain that only answers ping — everything else errors.
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
    let runner = TurnRunner::with_max_rounds(brain, dir.path(), 3);
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![]));

    let err = runner
        .run_turn("hello", &route_result(), "cli", "", caller.clone())
        .await
        .expect_err("prepare fails");

    assert!(err.to_string().contains("boom"), "{err}");
    let trace_path = dir.path().join("personal").join("logs").join("trace.jsonl");
    let line = std::fs::read_to_string(&trace_path).expect("trace written");
    let record: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(record["error"], "BrainError");
    assert_eq!(record["channel"], "cli");
    assert_eq!(record["scope"], "cli");
}

#[tokio::test]
async fn successful_turns_write_a_trace_line_with_the_routing_fields() {
    let dir = tempfile::tempdir().unwrap();
    let (runner, _methods) = started_runner(dir.path(), agent_prepare(), &[]).await;
    let caller = std::sync::Arc::new(model_loop::testing::FakeModelCaller::new(vec![
        final_result("ok"),
    ]));

    runner
        .run_turn("hello", &route_result(), "telegram", "42", caller.clone())
        .await
        .expect("turn succeeds");

    let trace_path = dir.path().join("personal").join("logs").join("trace.jsonl");
    let line = std::fs::read_to_string(&trace_path).expect("trace written");
    let record: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(record["channel"], "telegram");
    assert_eq!(record["scope"], "telegram-42");
    assert_eq!(record["tier"], 0);
    assert_eq!(record["model"], "claude-cli:fake");
    assert_eq!(record["provider"], "fake");
    assert_eq!(record["error"], serde_json::Value::Null);
    assert!(record["duration_ms"].as_f64().unwrap() >= 0.0);
}

#[test]
fn scope_for_slugs_channel_and_sender() {
    assert_eq!(scope_for("", ""), "default");
    assert_eq!(scope_for("telegram", "42"), "telegram-42");
    assert_eq!(scope_for("Telegram", "User@1"), "telegram-user-1");
    assert_eq!(scope_for("  ", ""), "default");
}
