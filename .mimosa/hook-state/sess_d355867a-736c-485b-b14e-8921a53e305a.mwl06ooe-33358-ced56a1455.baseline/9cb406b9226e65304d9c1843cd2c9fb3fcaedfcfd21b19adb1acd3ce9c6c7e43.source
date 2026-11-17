//! Failure classification and backoff for the claude CLI backend.
//! Port of the `classify_failure` / `_backoff_seconds` half of
//! `brain/nerv/llm/claude_cli.py`.

use std::time::{SystemTime, UNIX_EPOCH};

use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;

use crate::llm::truncate;

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

/// Exponential backoff capped at 8s (1s, 2s, 4s, 8s) with +/-10% jitter.
pub fn backoff_seconds(attempt: u32) -> f64 {
    let base = 2u32.saturating_pow(attempt.saturating_sub(1)).min(8) as f64;
    base * jitter()
}

/// Jitter in [0.9, 1.1) — wall-clock nanoseconds mixed with a call counter,
/// because consecutive calls can land in the same microsecond and a full RNG
/// dependency is not justified for backoff.
fn jitter() -> f64 {
    static CALL: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let n = CALL.fetch_add(1, Ordering::Relaxed);
    let seed = nanos ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    0.9 + (seed % 1000) as f64 / 1000.0 * 0.2
}
