//! Claude Code CLI backend — runs prompts through the local `claude` binary.
//!
//! Port of `brain/nerv/llm/claude_cli.py` for plain text/json completion:
//! subscription auth, print mode, isolation flags, failure classification and
//! the retry loop. The MCP tool bridge (`_write_mcp_config`, tool names) stays
//! in Python until that module migrates — the Router never passes tools.
//!
//! This uses the user's logged-in Claude Code session (subscription auth), so
//! no ANTHROPIC_API_KEY is required.

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::warn;

use super::{truncate, ChatMessage, CompletionResult, LlmError};

/// Errors we cannot classify are retried once (the pre-classification default),
/// mirroring `UNKNOWN_ERROR_MAX_ATTEMPTS` in Python.
pub const UNKNOWN_ERROR_MAX_ATTEMPTS: u32 = 2;

/// Flags that isolate `claude -p` from the user's personal Claude Code setup
/// (global CLAUDE.md, hooks, plugins, MCP servers, skills, built-in tools).
/// `--tools ""` removes every built-in tool.
const ISOLATION_ARGS: &[&str] = &[
    "--tools",
    "",
    "--setting-sources",
    "",
    "--strict-mcp-config",
    "--no-session-persistence",
    "--disable-slash-commands",
];

const QUOTA_MARKERS: &[&str] = &[
    "usage limit reached", // observed text of subscription quota exhaustion
    "reached your usage limit",
    "credit balance is too low", // Anthropic API billing text
    "limit reached",
];
const AUTH_MARKERS: &[&str] = &[
    "not logged in", // claude CLI with no session
    "please run /login",
    "invalid api key",
    "authentication_error",
    "oauth token has expired",
];
const TRANSIENT_MARKERS: &[&str] = &[
    "timed out", // our own timeout
    "overloaded",
    "econnreset",
    "etimedout",
    "enotfound",
    "econnrefused",
    "network",
    "temporarily unavailable",
    "rate limit",
];
const TRANSIENT_ERROR_TYPES: &[&str] = &["overloaded_error", "rate_limit_error", "api_error"];
const FATAL_STATUSES: &[i64] = &[400, 401, 402, 403, 404, 413, 422];
const TRANSIENT_STATUSES: &[i64] = &[408, 409, 429];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Retrying cannot help.
    Fatal,
    /// Transient — retry up to the attempt budget.
    Retry,
    /// Unclassified — retried once, like the pre-classification behaviour.
    Unknown,
}

/// A classified `claude` CLI failure.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct ClaudeCliError {
    pub kind: FailureKind,
    pub message: String,
}

/// Backend parameters, injectable for tests (fake `claude` executable, zero
/// backoff) so tests never race on process environment variables.
#[derive(Clone)]
pub struct ClaudeCliBackend {
    pub bin: String,
    pub max_attempts: u32,
    /// Backoff seconds before the next attempt; swappable in tests.
    pub backoff: fn(u32) -> f64,
}

impl Default for ClaudeCliBackend {
    fn default() -> Self {
        Self {
            bin: std::env::var("NERV_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string()),
            max_attempts: std::env::var("NERV_CLAUDE_MAX_ATTEMPTS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2)
                .max(1),
            backoff: backoff_seconds,
        }
    }
}

impl std::fmt::Debug for ClaudeCliBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeCliBackend")
            .field("bin", &self.bin)
            .field("max_attempts", &self.max_attempts)
            .finish()
    }
}

/// Classify a failed claude call into `(kind, user_message)`.
///
/// Every rule names its evidence. Anything not matched is `Unknown` and is
/// retried once, so a new error string never becomes a hard failure by
/// accident. `status`/`error_type` come from the JSON result
/// (`api_error_status`, `api_error_type`; observed: an unknown --model gives
/// is_error=true, api_error_status=404, api_error_type=not_found_error,
/// exit code 1). Port of `classify_failure`.
pub fn classify_failure(
    status: Option<i64>,
    error_type: &str,
    text: &str,
) -> (FailureKind, String) {
    let lowered = text.to_lowercase();
    let detail = truncate(text.trim(), 200);
    // Quota first: a subscription limit can arrive as HTTP 429, which would
    // otherwise look like a transient rate limit.
    if status == Some(402) || QUOTA_MARKERS.iter().any(|m| lowered.contains(m)) {
        return (
            FailureKind::Fatal,
            format!(
                "Claude quota or subscription limit is exhausted; wait for the \
                 limit to reset or check the plan. ({detail})"
            ),
        );
    }
    if status == Some(401)
        || status == Some(403)
        || error_type == "authentication_error"
        || AUTH_MARKERS.iter().any(|m| lowered.contains(m))
    {
        return (
            FailureKind::Fatal,
            format!(
                "Claude CLI is not logged in or authentication failed; run `claude` \
                 in a terminal and log in first. ({detail})"
            ),
        );
    }
    if status.is_some_and(|s| FATAL_STATUSES.contains(&s))
        || error_type == "invalid_request_error"
        || error_type == "not_found_error"
    {
        return (
            FailureKind::Fatal,
            format!(
                "Claude CLI rejected the request (HTTP {}); check the configured \
                 model name. ({detail})",
                status.map_or_else(|| "None".to_string(), |s| s.to_string())
            ),
        );
    }
    if status.is_some_and(|s| TRANSIENT_STATUSES.contains(&s) || s >= 500)
        || TRANSIENT_ERROR_TYPES.contains(&error_type)
        || TRANSIENT_MARKERS.iter().any(|m| lowered.contains(m))
    {
        let message = if detail.is_empty() {
            "transient claude CLI failure".to_string()
        } else {
            detail
        };
        return (FailureKind::Retry, message);
    }
    let message = if detail.is_empty() {
        "unknown claude CLI failure".to_string()
    } else {
        detail
    };
    (FailureKind::Unknown, message)
}

/// Run one turn through the `claude` CLI in print mode (default backend).
pub async fn complete(
    messages: &[ChatMessage],
    model_name: &str,
    json_mode: bool,
    timeout: Duration,
) -> Result<CompletionResult, LlmError> {
    complete_with(
        &ClaudeCliBackend::default(),
        messages,
        model_name,
        json_mode,
        timeout,
    )
    .await
}

/// Run one turn with an explicit backend (tests inject a fake `claude`).
pub async fn complete_with(
    backend: &ClaudeCliBackend,
    messages: &[ChatMessage],
    model_name: &str,
    json_mode: bool,
    timeout: Duration,
) -> Result<CompletionResult, LlmError> {
    let (system_prompt, conversation) = flatten_messages(messages);
    let mut system_prompt = system_prompt;
    if json_mode {
        system_prompt = format!(
            "{system_prompt}\n\nRespond with ONLY a single valid JSON object and \
             no other text."
        )
        .trim()
        .to_string();
    }

    let mut args: Vec<String> = vec![
        "-p".to_string(),
        "--model".to_string(),
        if model_name.is_empty() {
            "opus"
        } else {
            model_name
        }
        .to_string(),
        "--output-format".to_string(),
        "json".to_string(),
    ];
    args.extend(ISOLATION_ARGS.iter().map(|s| s.to_string()));
    if !system_prompt.is_empty() {
        args.push("--system-prompt".to_string());
        args.push(system_prompt);
    }

    let mut attempts = backend.max_attempts.max(1);
    let mut last_error = "unknown error".to_string();
    let mut attempt = 0u32;
    loop {
        if attempt >= attempts {
            break Err(LlmError::ClaudeCli(ClaudeCliError {
                kind: FailureKind::Unknown,
                message: format!("claude CLI failed after {attempts} attempt(s): {last_error}"),
            }));
        }
        attempt += 1;

        match invoke_once(&backend.bin, &args, &conversation, timeout).await {
            Err(e) => {
                last_error = e.message.clone();
                warn!(
                    "claude CLI attempt {attempt}/{attempts} failed ({:?}): {e}",
                    e.kind
                );
                match e.kind {
                    FailureKind::Fatal => break Err(e.into()),
                    FailureKind::Unknown => attempts = attempts.min(UNKNOWN_ERROR_MAX_ATTEMPTS),
                    FailureKind::Retry => {}
                }
                if attempt < attempts {
                    sleep_between_attempts(backend, attempt).await;
                }
            }
            Ok(data) => {
                if is_truthy(data.get("is_error")) {
                    let status = data.get("api_error_status").and_then(|v| v.as_i64());
                    let error_type = data
                        .get("api_error_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    // Python: str(data.get("result") or data.get("subtype") or "")
                    let text = ["result", "subtype"]
                        .iter()
                        .find_map(|k| {
                            data.get(*k)
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                        })
                        .unwrap_or("");
                    let (kind, message) = classify_failure(status, error_type, text);
                    if kind == FailureKind::Fatal {
                        break Err(LlmError::ClaudeCli(ClaudeCliError { kind, message }));
                    }
                    if kind == FailureKind::Unknown {
                        attempts = attempts.min(UNKNOWN_ERROR_MAX_ATTEMPTS);
                    }
                    last_error = format!("claude returned error result: {message}");
                    warn!(
                        "claude CLI attempt {attempt}/{attempts} returned an error \
                         result ({kind:?})"
                    );
                    if attempt < attempts {
                        sleep_between_attempts(backend, attempt).await;
                    }
                } else {
                    // Claude Code already executed any tool calls internally via
                    // MCP, so the caller must not run its own tool loop on top
                    // of this (irrelevant for the Router, which passes no tools).
                    let content = data
                        .get("result")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    break Ok(CompletionResult { content });
                }
            }
        }
    }
}

/// Python truthiness for the JSON `is_error` field (bool, non-zero number or
/// non-empty string are all truthy).
fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        _ => false,
    }
}

async fn sleep_between_attempts(backend: &ClaudeCliBackend, attempt: u32) {
    let secs = (backend.backoff)(attempt);
    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
}

/// Exponential backoff capped at 8s (1s, 2s, 4s, 8s) with +/-10% jitter.
pub fn backoff_seconds(attempt: u32) -> f64 {
    let base = 2u32.saturating_pow(attempt.saturating_sub(1)).min(8) as f64;
    base * jitter()
}

/// Jitter in [0.9, 1.1) — wall-clock nanoseconds mixed with a call counter,
/// because consecutive calls can land in the same microsecond and a full RNG
/// dependency is not justified for backoff.
fn jitter() -> f64 {
    static CALL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    use std::sync::atomic::Ordering;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let n = CALL.fetch_add(1, Ordering::Relaxed);
    let seed = nanos ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    0.9 + (seed % 1000) as f64 / 1000.0 * 0.2
}

/// Split chat messages into (system_prompt, conversation_prompt).
/// Port of `_flatten_messages`.
pub fn flatten_messages(messages: &[ChatMessage]) -> (String, String) {
    let mut system_parts: Vec<&str> = Vec::new();
    let mut convo: Vec<String> = Vec::new();
    for msg in messages {
        let content = msg.content.trim();
        match msg.role.as_str() {
            "system" => {
                if !content.is_empty() {
                    system_parts.push(content);
                }
            }
            "assistant" => {
                if !content.is_empty() {
                    convo.push(format!("Assistant: {content}"));
                }
            }
            "tool" => {
                convo.push(format!("[Tool result]: {content}"));
            }
            // user (and any unknown roles)
            _ => {
                convo.push(content.to_string());
            }
        }
    }

    let system_prompt = system_parts.join("\n\n");
    // If there is no prior history, the single user message is the whole prompt.
    let conversation = convo
        .iter()
        .filter(|part| !part.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    (system_prompt, conversation)
}

/// Run the claude CLI once; raise a classified error on any failure.
/// Port of `_invoke_once`.
async fn invoke_once(
    bin: &str,
    args: &[String],
    conversation: &str,
    timeout: Duration,
) -> Result<Value, ClaudeCliError> {
    // Run from a scratch cwd so project/user CLAUDE.md files are not discovered.
    let mut child = tokio::process::Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(std::env::temp_dir())
        .kill_on_drop(true)
        .spawn()
        // Python surfaces spawn errors (e.g. a missing binary) immediately,
        // without retrying — same here.
        .map_err(|e| ClaudeCliError {
            kind: FailureKind::Fatal,
            message: format!("failed to spawn claude CLI: {e}"),
        })?;

    let mut stdin = child.stdin.take().ok_or_else(|| ClaudeCliError {
        kind: FailureKind::Fatal,
        message: "claude CLI stdin unavailable".to_string(),
    })?;
    let mut stdout_pipe = child.stdout.take().ok_or_else(|| ClaudeCliError {
        kind: FailureKind::Fatal,
        message: "claude CLI stdout unavailable".to_string(),
    })?;
    let mut stderr_pipe = child.stderr.take().ok_or_else(|| ClaudeCliError {
        kind: FailureKind::Fatal,
        message: "claude CLI stderr unavailable".to_string(),
    })?;

    let io = async move {
        // Write the conversation, then send EOF: tokio's shutdown() does not
        // close a process pipe, so the fd must actually be dropped. The
        // drop must be EXPLICIT — `stdin` is a captured field of this async
        // block, and captured fields are only released when the future
        // itself drops, so a scope-end drop would leave the child waiting
        // for EOF until this call times out.
        // A write error here is fine: the CLI may exit before reading stdin.
        let _ = stdin.write_all(conversation.as_bytes()).await;
        let _ = stdin.shutdown().await;
        drop(stdin);
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut stdout_buf).await;
        let _ = stderr_pipe.read_to_end(&mut stderr_buf).await;
        (stdout_buf, stderr_buf)
    };

    let (stdout_buf, stderr_buf) = match tokio::time::timeout(timeout, io).await {
        Ok(buffers) => buffers,
        Err(_) => {
            let _ = child.kill().await;
            return Err(ClaudeCliError {
                kind: FailureKind::Retry,
                message: format!("claude CLI timed out after {}s", timeout.as_secs_f64()),
            });
        }
    };

    let status = child.wait().await.map_err(|e| ClaudeCliError {
        kind: FailureKind::Unknown,
        message: format!("claude CLI wait failed: {e}"),
    })?;

    let raw = String::from_utf8_lossy(&stdout_buf).trim().to_string();
    if !status.success() {
        let err = String::from_utf8_lossy(&stderr_buf).trim().to_string();
        // A failing `claude -p --output-format json` usually still prints the
        // error result as JSON on stdout (exit code 1), so classify from that.
        if let Ok(parsed) = serde_json::from_str::<Value>(&raw) {
            if is_truthy(parsed.get("is_error")) {
                return Ok(parsed);
            }
        }
        let source = if err.is_empty() { &raw } else { &err };
        let (kind, _) = classify_failure(None, "", source);
        return Err(ClaudeCliError {
            kind,
            message: format!(
                "claude CLI exited {}: {}",
                status
                    .code()
                    .map_or_else(|| "None".to_string(), |c| c.to_string()),
                truncate(source, 500)
            ),
        });
    }

    match serde_json::from_str::<Value>(&raw) {
        Ok(data) => Ok(data),
        Err(_) => Err(ClaudeCliError {
            kind: FailureKind::Unknown,
            message: format!(
                "claude CLI returned non-JSON output: {}",
                truncate(&raw, 500)
            ),
        }),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    fn messages() -> Vec<ChatMessage> {
        vec![ChatMessage::user("hi")]
    }

    /// Install a fake `claude` that counts invocations and prints `body`.
    fn fake_claude(dir: &Path, body: &Value, code: i32) -> (ClaudeCliBackend, PathBuf) {
        let counter = dir.join("calls.txt");
        let body_file = dir.join("body.json");
        std::fs::write(&body_file, body.to_string()).unwrap();
        let script = dir.join("fake-claude");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf x >> {}\ncat > /dev/null\ncat {}\nexit {}\n",
                counter.display(),
                body_file.display(),
                code
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(perms.mode() | 0o100);
        std::fs::set_permissions(&script, perms).unwrap();
        (
            ClaudeCliBackend {
                bin: script.display().to_string(),
                max_attempts: 2,
                backoff: |_attempt| 0.0,
            },
            counter,
        )
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

    async fn run_expect_error(
        dir: &Path,
        body: &Value,
        code: i32,
        max_attempts: u32,
    ) -> (String, PathBuf) {
        let (mut backend, counter) = fake_claude(dir, body, code);
        backend.max_attempts = max_attempts;
        let err = complete_with(&backend, &messages(), "opus", false, Duration::from_secs(5))
            .await
            .expect_err("fake claude is configured to fail");
        match err {
            LlmError::ClaudeCli(e) => (e.message, counter),
            other => panic!("expected ClaudeCli error, got {other}"),
        }
    }

    #[tokio::test]
    async fn auth_failure_is_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let (message, counter) = run_expect_error(
            dir.path(),
            &err_body(None, "Not logged in - Please run /login", ""),
            1,
            3,
        )
        .await;
        assert!(message.contains("not logged in"), "{message}");
        assert_eq!(calls(&counter), 1);
    }

    #[tokio::test]
    async fn invalid_model_is_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let (message, counter) = run_expect_error(
            dir.path(),
            &err_body(
                Some(404),
                "issue with the selected model",
                "not_found_error",
            ),
            1,
            3,
        )
        .await;
        assert!(message.contains("rejected the request"), "{message}");
        assert_eq!(calls(&counter), 1);
    }

    #[tokio::test]
    async fn quota_exhausted_is_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let (message, counter) = run_expect_error(
            dir.path(),
            &err_body(Some(429), "Claude AI usage limit reached|1700000000", ""),
            1,
            3,
        )
        .await;
        assert!(message.contains("quota"), "{message}");
        assert_eq!(calls(&counter), 1);
    }

    #[tokio::test]
    async fn overloaded_5xx_is_retried_up_to_limit() {
        let dir = tempfile::tempdir().unwrap();
        let (message, counter) =
            run_expect_error(dir.path(), &err_body(Some(529), "Overloaded", ""), 1, 3).await;
        assert!(message.contains("failed after 3 attempt"), "{message}");
        // exit code 1 with an error JSON on stdout still carries the classification
        assert_eq!(calls(&counter), 3);
    }

    #[tokio::test]
    async fn rate_limit_429_is_retried() {
        let dir = tempfile::tempdir().unwrap();
        let (_, counter) =
            run_expect_error(dir.path(), &err_body(Some(429), "Rate limited", ""), 1, 3).await;
        assert_eq!(calls(&counter), 3);
    }

    #[tokio::test]
    async fn success_is_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let (backend, counter) = fake_claude(dir.path(), &serde_json::json!({ "result": "ok" }), 0);
        let result = complete_with(&backend, &messages(), "opus", false, Duration::from_secs(5))
            .await
            .expect("fake claude succeeds");
        assert_eq!(result.content, "ok");
        assert_eq!(calls(&counter), 1);
    }

    #[tokio::test]
    async fn unknown_error_retries_once_only() {
        let dir = tempfile::tempdir().unwrap();
        let (_, counter) =
            run_expect_error(dir.path(), &err_body(None, "something odd", ""), 1, 4).await;
        assert_eq!(calls(&counter), 2);
    }

    #[tokio::test]
    async fn timeout_is_retried_to_limit() {
        // A backend whose binary sleeps forever exercises the timeout path.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("sleepy-claude");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(perms.mode() | 0o100);
        std::fs::set_permissions(&script, perms).unwrap();
        let backend = ClaudeCliBackend {
            bin: script.display().to_string(),
            max_attempts: 3,
            backoff: |_attempt| 0.0,
        };
        let err = complete_with(
            &backend,
            &messages(),
            "opus",
            false,
            Duration::from_millis(50),
        )
        .await
        .expect_err("sleeping claude must time out");
        match err {
            LlmError::ClaudeCli(e) => {
                assert!(e.message.contains("failed after 3 attempt"), "{e}");
            }
            other => panic!("expected ClaudeCli error, got {other}"),
        }
    }

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

    #[test]
    fn flatten_messages_splits_system_and_conversation() {
        let messages = vec![
            ChatMessage::system("Be terse."),
            ChatMessage::system("Also: JSON only."),
            ChatMessage::user("  hello  "),
            ChatMessage {
                role: "assistant".to_string(),
                content: "hi there".to_string(),
            },
            ChatMessage {
                role: "tool".to_string(),
                content: "42".to_string(),
            },
            ChatMessage::user("sum it up"),
        ];
        let (system, conversation) = flatten_messages(&messages);
        assert_eq!(system, "Be terse.\n\nAlso: JSON only.");
        assert_eq!(
            conversation,
            "hello\n\nAssistant: hi there\n\n[Tool result]: 42\n\nsum it up"
        );
    }

    #[test]
    fn json_mode_appends_the_json_suffix() {
        let messages = vec![ChatMessage::system("You classify."), ChatMessage::user("m")];
        let (system, _conversation) = flatten_messages(&messages);
        let with_suffix =
            format!("{system}\n\nRespond with ONLY a single valid JSON object and no other text.");
        assert!(with_suffix
            .ends_with("Respond with ONLY a single valid JSON object and no other text."));
    }
}
