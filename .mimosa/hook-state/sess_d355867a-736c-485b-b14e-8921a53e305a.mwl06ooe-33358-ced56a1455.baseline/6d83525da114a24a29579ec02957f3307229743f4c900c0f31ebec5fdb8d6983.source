//! Rolling summaries for past conversation days.
//!
//! Port of `brain/nerv/memory/summarizer.py`. Only *completed* days are
//! summarised: today is still being appended to, so summarising it would be
//! redone on the next turn. A failed summary must never break the turn that
//! triggered it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing::{info, warn};

use super::manager::MemoryManager;
use crate::llm::ChatMessage;
use crate::turn::model_loop::ModelCaller;

/// Enough of a day to summarise faithfully without paying for a huge prompt.
pub const MAX_CHARS_PER_DAY: usize = 12000;

const PROMPT: &str = "Summarise this day of conversation between a user and their assistant.\n\n\
Write 2-4 sentences capturing what was decided, what the user asked for, and \
any durable detail worth recalling weeks later (names, versions, paths, \
preferences). Omit pleasantries. Write plain prose, no bullet points, no \
preamble — output only the summary itself.\n\nConversation:\n{body}";

/// `(conversation dir, date)` pairs currently being summarised in this
/// process. Overlapping runs leave a day alone instead of paying for a
/// duplicate model call.
fn in_flight() -> &'static Mutex<HashSet<(PathBuf, String)>> {
    static IN_FLIGHT: std::sync::LazyLock<Mutex<HashSet<(PathBuf, String)>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashSet::new()));
    &IN_FLIGHT
}

/// Flatten one day's messages into prompt text, oldest first.
pub(crate) fn render_day(memory: &MemoryManager, date_str: &str) -> String {
    let conv = memory.load_conversation(date_str);
    let lines: Vec<String> = conv
        .messages
        .iter()
        .map(|m| {
            format!(
                "{}: {}",
                m.role,
                m.content.split_whitespace().collect::<Vec<_>>().join(" ")
            )
        })
        .collect();
    let mut body = lines.join("\n");
    if body.chars().count() > MAX_CHARS_PER_DAY {
        // Keep the end of the day: conclusions matter more than opening chatter.
        body = format!(
            "...[earlier turns omitted]\n{}",
            body.chars()
                .skip(body.chars().count() - MAX_CHARS_PER_DAY)
                .collect::<String>()
        );
    }
    body
}

/// Generate and store the summary for one day. Returns it, or "" on failure.
pub async fn summarize_day(
    memory: &MemoryManager,
    date_str: &str,
    caller: &dyn ModelCaller,
    model: &str,
    _project_root: Option<&Path>,
) -> String {
    let key = (memory.conv_dir.clone(), date_str.to_string());
    // Idempotent: a day already summarised, or being summarised by an
    // overlapping run, is left alone instead of paying for a duplicate call.
    if in_flight().lock().unwrap().contains(&key) {
        return String::new();
    }
    let existing = memory.read_summary(date_str);
    if !existing.is_empty() {
        return existing;
    }
    in_flight().lock().unwrap().insert(key.clone());
    let result = summarize_day_inner(memory, date_str, caller, model, _project_root).await;
    in_flight().lock().unwrap().remove(&key);
    result
}

async fn summarize_day_inner(
    memory: &MemoryManager,
    date_str: &str,
    caller: &dyn ModelCaller,
    model: &str,
    _project_root: Option<&Path>,
) -> String {
    let body = render_day(memory, date_str);
    if body.trim().is_empty() {
        return String::new();
    }

    let prompt = PROMPT.replace("{body}", &body);
    let result = caller
        .complete(&[ChatMessage::user(prompt)], model, None, &[], "")
        .await;

    let summary = match result {
        Ok(result) => result.content.trim().to_string(),
        Err(e) => {
            warn!("Failed to summarise {date_str}: {e}");
            return String::new();
        }
    };
    if summary.is_empty() {
        return String::new();
    }

    memory.write_summary(date_str, &summary);
    info!(
        "Summarised conversation day {date_str} ({} chars)",
        summary.len()
    );
    summary
}

/// Summarise up to `limit` past days that do not have a summary yet.
///
/// Bounded per call so a long-dormant install catches up over several turns
/// instead of stalling one on a burst of model calls.
pub async fn backfill_summaries(
    memory: &MemoryManager,
    caller: &dyn ModelCaller,
    model: &str,
    _project_root: Option<&Path>,
    limit: usize,
) -> usize {
    let mut written = 0;
    for date_str in memory.days_needing_summary(limit) {
        if !summarize_day(memory, &date_str, caller, model, _project_root)
            .await
            .is_empty()
        {
            written += 1;
        }
    }
    written
}

/// The shared caller type the facade holds (production `ProviderCaller`).
pub type ArcCaller = std::sync::Arc<dyn ModelCaller>;

/// Spawn the fire-and-forget backfill the python side used to schedule after
/// each turn: never awaited on the reply path.
pub fn spawn_backfill(project_root: PathBuf, model: String, caller: ArcCaller) {
    tokio::spawn(async move {
        let memory = MemoryManager::new(&project_root, super::manager::DEFAULT_SCOPE);
        let _ = backfill_summaries(&memory, caller.as_ref(), &model, Some(&project_root), 1).await;
    });
}
