//! One `claude -p` subprocess invocation and the message flattening that
//! feeds it. Port of `_invoke_once` / `_flatten_messages` in
//! `brain/nerv/llm/claude_cli.py`.

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::classify::{classify_failure, ClaudeCliError, FailureKind};
use crate::llm::truncate;
use crate::llm::ChatMessage;

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
pub async fn invoke_once(
    bin: &str,
    args: &[String],
    conversation: &str,
    timeout: Duration,
) -> Result<Value, ClaudeCliError> {
    // Run from a scratch cwd so project/user CLAUDE.md files are not discovered.
    // ETXTBSY can transiently fire right after a timed-out child was killed
    // (the dying process still holds the executable during exit) — retry the
    // spawn itself briefly before treating it as fatal.
    let mut child = None;
    for attempt in 0..3 {
        match tokio::process::Command::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(std::env::temp_dir())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => {
                child = Some(c);
                break;
            }
            Err(e) if e.raw_os_error() == Some(26) && attempt < 2 => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            // Python surfaces spawn errors (e.g. a missing binary) immediately,
            // without retrying — same here for anything but ETXTBSY.
            Err(e) => {
                return Err(ClaudeCliError {
                    kind: FailureKind::Fatal,
                    message: format!("failed to spawn claude CLI: {e}"),
                });
            }
        }
    }
    let mut child = child.expect("spawn loop exits with a child or an error");

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
            if super::is_truthy(parsed.get("is_error")) {
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
