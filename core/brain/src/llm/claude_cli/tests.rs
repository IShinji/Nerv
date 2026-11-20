//! Tests ported from `brain/tests/test_claude_cli.py` (retry, metadata,
//! message flattening, MCP bridge) plus the failure-classification table from
//! `test_claude_cli_classify.py`. Fake `claude` executables are `/bin/sh`
//! scripts, mirroring the Python tests' fake-executable technique.

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn messages() -> Vec<ChatMessage> {
    vec![ChatMessage::user("hi")]
}

fn zero_backoff_backend(script: &Path, max_attempts: u32) -> ClaudeCliBackend {
    ClaudeCliBackend {
        bin: script.display().to_string(),
        max_attempts,
        backoff: |_attempt| 0.0,
    }
}

fn make_executable(script: &Path) {
    let mut perms = std::fs::metadata(script).unwrap().permissions();
    perms.set_mode(perms.mode() | 0o100);
    std::fs::set_permissions(script, perms).unwrap();
}

fn request<'a>(
    model_name: &'a str,
    json_mode: bool,
    tool_names: &'a [String],
    project_root: Option<&'a Path>,
    scope: &'a str,
) -> ClaudeCliRequest<'a> {
    ClaudeCliRequest {
        model_name,
        json_mode,
        tool_names,
        project_root,
        scope,
        progress: None,
    }
}

/// A fake `claude` that counts invocations and prints a per-call body from a
/// responses file (one JSON line per call, last line repeats).
fn fake_claude(dir: &Path, responses: &[Value]) -> (ClaudeCliBackend, PathBuf) {
    let counter = dir.join("calls.txt");
    let responses_file = dir.join("responses.jsonl");
    let body: String = responses
        .iter()
        .map(|v| serde_json::to_string(v).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&responses_file, body).unwrap();
    let script = dir.join("fake-claude");
    // Print the Nth line for the Nth call (the last line repeats if called
    // more often than there are responses), and log every argument.
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
printf x >> {counter}
cat > /dev/null
n=$(cat {counter} | wc -c | tr -d ' ')
total=$(wc -l < {responses_file} | tr -d ' ')
if [ "$n" -gt "$total" ]; then n=$total; fi
sed -n "${{n}}p" {responses_file}
prev=""
for a in "$@"; do
  if [ "$prev" = "--mcp-config" ]; then cp "$a" {mcp_capture} 2>/dev/null; fi
  printf '%s\n' "$a" >> {args_capture}
  prev=$a
done
exit 0
"#,
            counter = counter.display(),
            responses_file = responses_file.display(),
            mcp_capture = dir.join("mcp_capture.json").display(),
            args_capture = dir.join("args.txt").display(),
        ),
    )
    .unwrap();
    make_executable(&script);
    (zero_backoff_backend(&script, 2), counter)
}

fn calls(counter: &Path) -> usize {
    std::fs::read_to_string(counter)
        .map(|s| s.len())
        .unwrap_or(0)
}

fn err_body(status: Option<i64>, text: &str, error_type: &str) -> Value {
    serde_json::json!({
        "is_error": true,
        "subtype": "success",
        "api_error_status": status,
        "api_error_type": error_type,
        "result": text,
    })
}

async fn claude_cli_run(
    backend: &ClaudeCliBackend,
    opts: &ClaudeCliRequest<'_>,
) -> Result<CompletionResult, LlmError> {
    complete_with(backend, &messages(), opts, Duration::from_secs(5)).await
}

// --- flatten_messages ---

#[test]
fn flatten_messages_splits_system_and_conversation() {
    let (system, convo) = flatten_messages(&[
        ChatMessage::system("You are Nerv."),
        ChatMessage::user("hi"),
        ChatMessage::assistant("hello"),
        ChatMessage::tool("result data"),
    ]);
    assert_eq!(system, "You are Nerv.");
    assert!(convo.contains("hi"));
    assert!(convo.contains("Assistant: hello"));
    assert!(convo.contains("[Tool result]: result data"));
}

// --- retry + metadata (port of test_retries_then_succeeds etc.) ---

#[tokio::test]
async fn retries_then_succeeds_and_passes_metadata_through() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, counter) = fake_claude(
        dir.path(),
        &[
            err_body(Some(529), "Overloaded", ""),
            serde_json::json!({"result": "ok", "duration_ms": 5.0, "total_cost_usd": 0.01, "usage": {"x": 1}}),
        ],
    );
    let backend = ClaudeCliBackend {
        max_attempts: 3,
        ..backend
    };
    let opts = request("opus", false, &[], None, "");
    let result = claude_cli_run(&backend, &opts)
        .await
        .expect("second call succeeds");
    assert_eq!(result.content, "ok");
    assert!(result.runs_own_tool_loop);
    assert_eq!(result.cost_usd, Some(0.01));
    assert_eq!(result.duration_ms, Some(5.0));
    assert_eq!(result.usage.as_ref().unwrap()["x"], 1);
    assert_eq!(calls(&counter), 2);
}

#[tokio::test]
async fn raises_after_exhausting_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, counter) = fake_claude(dir.path(), &[err_body(Some(529), "Overloaded", "")]);
    let backend = ClaudeCliBackend {
        max_attempts: 2,
        ..backend
    };
    let opts = request("opus", false, &[], None, "");
    let err = claude_cli_run(&backend, &opts)
        .await
        .expect_err("always failing");
    assert!(err.to_string().contains("failed after 2 attempt"), "{err}");
    assert_eq!(calls(&counter), 2);
}

#[tokio::test]
async fn error_result_retried() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, _counter) = fake_claude(
        dir.path(),
        &[
            serde_json::json!({"is_error": true, "subtype": "overloaded", "result": ""}),
            serde_json::json!({"result": "recovered", "duration_ms": 3.0}),
        ],
    );
    let opts = request("opus", false, &[], None, "");
    let result = claude_cli_run(&backend, &opts)
        .await
        .expect("second phase recovers");
    assert_eq!(result.content, "recovered");
}

// --- isolation flags (port of test_args_isolate_from_user_environment) ---

#[tokio::test]
async fn args_isolate_from_user_environment() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, _counter) = fake_claude(dir.path(), &[serde_json::json!({"result": "ok"})]);
    let opts = request("opus", false, &[], None, "");
    claude_cli_run(&backend, &opts).await.expect("succeeds");

    let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
    let lines: Vec<&str> = args.lines().collect();
    assert!(lines.contains(&"--strict-mcp-config"));
    assert!(lines.contains(&"--no-session-persistence"));
    let tools_idx = lines.iter().position(|l| *l == "--tools").unwrap();
    assert_eq!(lines[tools_idx + 1], "");
    let sources_idx = lines
        .iter()
        .position(|l| *l == "--setting-sources")
        .unwrap();
    assert_eq!(lines[sources_idx + 1], "");
    assert!(!lines.contains(&"--disallowedTools"));
}

// --- MCP bridge ---

#[tokio::test]
async fn tool_names_bridge_through_the_mcp_config() {
    let dir = tempfile::tempdir().unwrap();
    let project_root = tempfile::tempdir().unwrap();
    let (backend, _counter) = fake_claude(dir.path(), &[serde_json::json!({"result": "ok"})]);
    let tool_names = vec!["local.exec".to_string(), "browser.read".to_string()];
    let opts = request(
        "opus",
        false,
        &tool_names,
        Some(project_root.path()),
        "telegram:42",
    );
    claude_cli_run(&backend, &opts).await.expect("succeeds");

    // The provider names resolve through the capability alias table and are
    // passed as mcp__nerv__<name> allowed tools.
    let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
    let lines: Vec<&str> = args.lines().collect();
    let mcp_idx = lines.iter().position(|l| *l == "--mcp-config").unwrap();
    assert!(lines.contains(&"mcp__nerv__shell"));
    assert!(lines.contains(&"mcp__nerv__browser"));
    assert!(!lines.contains(&"mcp__nerv__local.exec"));

    // The temp config survives long enough for the CLI to copy it and is
    // removed after the call (the Python `finally`).
    let capture = dir.path().join("mcp_capture.json");
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&capture).unwrap()).unwrap();
    // The rust server entry is this test binary itself in --mcp-server mode;
    // the python sidecar entry keeps the brain cwd.
    let server = &config["mcpServers"]["nerv"];
    assert!(
        server["command"].as_str().unwrap().contains("nerv_brain"),
        "{}",
        server["command"]
    );
    assert_eq!(server["args"][0], "--mcp-server");
    assert_eq!(server["env"]["NERV_MEMORY_SCOPE"], "telegram:42");
    assert_eq!(
        server["env"]["NERV_PROJECT_ROOT"].as_str(),
        Some(project_root.path().to_string_lossy().as_ref())
    );
    let mcp_path = PathBuf::from(lines[mcp_idx + 1]);
    assert!(!mcp_path.exists(), "temp MCP config must be removed");
    assert!(mcp_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with(MCP_CONFIG_PREFIX));
}

#[tokio::test]
async fn no_tool_names_means_no_mcp_config() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, _counter) = fake_claude(dir.path(), &[serde_json::json!({"result": "ok"})]);
    let opts = request("opus", false, &[], None, "");
    claude_cli_run(&backend, &opts).await.expect("succeeds");
    let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
    assert!(!args.contains("--mcp-config"));
    assert!(!dir.path().join("mcp_capture.json").exists());
}

#[test]
fn resolve_provider_names_maps_aliases_and_passes_tools_through() {
    let names = vec![
        "local.exec".to_string(),
        "shell".to_string(), // direct tool name: resolves to itself
        "browser.read".to_string(),
        "local.exec".to_string(),        // duplicate: deduped
        "something_unknown".to_string(), // passes through like Python
    ];
    assert_eq!(
        resolve_provider_names(&names),
        vec![
            "shell".to_string(),
            "browser".to_string(),
            "something_unknown".to_string()
        ]
    );
}

// --- timeout (port of test_timeout_kills_child_process) ---

#[tokio::test]
async fn timeout_kills_child_process() {
    let dir = tempfile::tempdir().unwrap();
    let alive_marker = dir.path().join("alive.txt");
    let script = dir.path().join("sleepy-claude");
    // If the child survives the timeout it writes the marker after its sleep.
    std::fs::write(
        &script,
        format!("#!/bin/sh\nsleep 1\ntouch {}\n", alive_marker.display()),
    )
    .unwrap();
    make_executable(&script);
    let backend = zero_backoff_backend(&script, 3);
    let opts = request("opus", false, &[], None, "");
    let err = complete_with(&backend, &messages(), &opts, Duration::from_millis(50))
        .await
        .expect_err("sleeping claude must time out");
    assert!(err.to_string().contains("timed out"), "{err}");
    assert!(!alive_marker.exists(), "the timed-out child must be killed");
}

// --- failure classification table (test_claude_cli_classify.py) ---

#[test]
fn classify_failure_table() {
    let cases: &[(Option<i64>, &str, FailureKind)] = &[
        (Some(401), "", FailureKind::Fatal),
        (None, "Invalid API key", FailureKind::Fatal),
        (Some(400), "bad", FailureKind::Fatal),
        (Some(402), "", FailureKind::Fatal),
        (Some(500), "", FailureKind::Retry),
        (Some(503), "", FailureKind::Retry),
        (None, "ECONNRESET while connecting", FailureKind::Retry),
        (None, "???", FailureKind::Unknown),
    ];
    for (status, text, expected) in cases {
        let (kind, _message) = classify_failure(*status, "", text);
        assert_eq!(kind, *expected, "status={status:?} text={text:?}");
    }
}

#[test]
fn backoff_has_bounded_jitter() {
    let values: std::collections::HashSet<u32> = (0..50)
        .map(|_| (backoff_seconds(2) * 100.0) as u32)
        .collect();
    assert!(values.len() > 1, "jitter must produce varying values");
    for v in values {
        let secs = v as f64 / 100.0;
        assert!((1.8..=2.2).contains(&secs), "{secs}");
    }
}
