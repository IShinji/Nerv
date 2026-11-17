//! Tests ported from `brain/tests/test_memory.py`, `test_memory_facts.py`,
//! `test_memory_search.py`, `test_memory_summaries.py` and
//! `test_summary_idempotent.py` — covering the rust memory manager, the
//! context assembly, and the summarizer. The python twins of these tests
//! keep covering the python manager the MCP tools still use.

use super::context::{FACTS_INLINE_TOKEN_BUDGET, SUMMARY_TOKEN_BUDGET};
use super::summarizer::MAX_CHARS_PER_DAY;
use super::*;
use crate::llm::ChatMessage;
use crate::llm::{CompletionResult, LlmError};
use crate::turn::model_loop::ModelCaller;
use chrono::Local;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

fn manager(tmp: &Path) -> MemoryManager {
    MemoryManager::new(tmp, DEFAULT_SCOPE)
}

fn day(manager: &MemoryManager, date: &str, pairs: &[(&str, &str)]) {
    manager.save_conversation(&DailyConversation {
        date: date.to_string(),
        messages: pairs
            .iter()
            .map(|(r, c)| StoredChatMessage::new(r, c))
            .collect(),
        summary: String::new(),
    });
}

// ── manager ─────────────────────────────────────────────────────────────────

#[test]
fn save_and_load_round_trips_daily_conversations() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());

    manager.save_conversation(&DailyConversation {
        date: "2026-04-11".to_string(),
        messages: vec![
            StoredChatMessage::new("user", "hello"),
            StoredChatMessage::new("assistant", "hi there"),
        ],
        summary: String::new(),
    });

    let file_path = manager.daily_file("2026-04-11");
    assert!(file_path.exists());
    let raw = std::fs::read_to_string(&file_path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2);
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["content"], "hello");
    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["content"], "hi there");

    let reloaded = manager.load_conversation("2026-04-11");
    let contents: Vec<&str> = reloaded
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(contents, ["hello", "hi there"]);
}

#[test]
fn load_recent_walks_backwards_across_days() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    manager.save_conversation(&DailyConversation {
        date: "2026-04-10".to_string(),
        messages: vec![
            StoredChatMessage::new("user", "day 1 msg 1"),
            StoredChatMessage::new("assistant", "day 1 msg 2"),
        ],
        summary: String::new(),
    });
    manager.save_conversation(&DailyConversation {
        date: "2026-04-11".to_string(),
        messages: vec![StoredChatMessage::new("user", "day 2 msg 1")],
        summary: String::new(),
    });

    let recent = manager.load_recent_messages(2);
    let contents: Vec<&str> = recent.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, ["day 1 msg 2", "day 2 msg 1"]);
}

#[test]
fn conversations_are_isolated_per_scope() {
    let dir = tempfile::tempdir().unwrap();
    let alice = MemoryManager::new(dir.path(), "telegram-alice");
    let bob = MemoryManager::new(dir.path(), "telegram-bob");

    alice.append_message("user", "my bank pin is 1234");
    bob.append_message("user", "hello");

    let alice_msgs: Vec<String> = alice
        .load_recent_messages(20)
        .into_iter()
        .map(|m| m.content)
        .collect();
    let bob_msgs: Vec<String> = bob
        .load_recent_messages(20)
        .into_iter()
        .map(|m| m.content)
        .collect();
    assert_eq!(alice_msgs, ["my bank pin is 1234"]);
    assert_eq!(bob_msgs, ["hello"]);
    assert_ne!(alice.conv_dir, bob.conv_dir);
}

#[test]
fn default_scope_keeps_legacy_paths() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    assert_eq!(
        manager.conv_dir,
        dir.path()
            .join("personal")
            .join("memory")
            .join("conversations")
    );
}

#[test]
fn legacy_json_day_is_read_and_migrated() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    let today = MemoryManager::today_str();
    let legacy = manager.conv_dir.join(format!("{today}.json"));
    let legacy_conv = DailyConversation {
        date: today.clone(),
        messages: vec![StoredChatMessage::new("user", "old turn")],
        summary: String::new(),
    };
    std::fs::write(&legacy, serde_json::to_string(&legacy_conv).unwrap()).unwrap();

    // Readable before any migration happens.
    let recent: Vec<String> = manager
        .load_recent_messages(20)
        .into_iter()
        .map(|m| m.content)
        .collect();
    assert_eq!(recent, ["old turn"]);

    manager.append_message("assistant", "new turn");

    assert!(!legacy.exists());
    assert!(manager.daily_file(&today).exists());
    let recent: Vec<String> = manager
        .load_recent_messages(20)
        .into_iter()
        .map(|m| m.content)
        .collect();
    assert_eq!(recent, ["old turn", "new turn"]);
}

// ── search ──────────────────────────────────────────────────────────────────

#[test]
fn search_finds_message_outside_recent_window() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    day(
        &manager,
        "2026-01-05",
        &[("user", "the telegram adapter drops long replies")],
    );
    for day_num in 10..25 {
        day(
            &manager,
            &format!("2026-02-{day_num}"),
            &[("user", "unrelated chatter")],
        );
    }

    // It is well past the 10-message recency window the context manager uses.
    let recent = manager.load_recent_messages(10);
    assert!(recent.iter().all(|m| !m.content.contains("telegram")));

    let hits = manager.search_messages("telegram adapter", 10, "", "", "");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].date, "2026-01-05");
}

#[test]
fn search_ranks_whole_phrase_above_scattered_terms() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    day(
        &manager,
        "2026-03-01",
        &[("user", "a rate limit on the upload path")],
    );
    day(
        &manager,
        "2026-03-02",
        &[("user", "the rate is fine but the limit is not")],
    );

    let hits = manager.search_messages("rate limit", 10, "", "", "");
    assert_eq!(hits[0].date, "2026-03-01");
    assert!(hits[0].score > hits[1].score);
}

#[test]
fn search_recency_breaks_score_ties() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    day(&manager, "2026-03-01", &[("user", "deploy the gateway")]);
    day(&manager, "2026-03-09", &[("user", "deploy the gateway")]);

    let hits = manager.search_messages("deploy the gateway", 10, "", "", "");
    let dates: Vec<&str> = hits.iter().map(|h| h.date.as_str()).collect();
    assert_eq!(dates, ["2026-03-09", "2026-03-01"]);
}

#[test]
fn search_filters_by_date_and_role() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    day(
        &manager,
        "2026-03-01",
        &[("user", "ship it"), ("assistant", "ship it")],
    );
    day(&manager, "2026-03-05", &[("user", "ship it")]);

    assert_eq!(manager.search_messages("ship it", 10, "", "", "").len(), 3);
    assert_eq!(
        manager
            .search_messages("ship it", 10, "2026-03-02", "", "")
            .len(),
        1
    );
    assert_eq!(
        manager
            .search_messages("ship it", 10, "", "2026-03-01", "")
            .len(),
        2
    );
    let roles = manager.search_messages("ship it", 10, "", "", "assistant");
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].message.role, "assistant");
}

#[test]
fn search_honours_limit_and_empty_query() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    let pairs: Vec<(&str, &str)> = std::iter::repeat_n(("user", "repeat"), 8).collect();
    day(&manager, "2026-03-01", &pairs);

    assert_eq!(manager.search_messages("repeat", 3, "", "", "").len(), 3);
    assert!(manager.search_messages("   ", 10, "", "", "").is_empty());
}

#[test]
fn search_never_crosses_scopes() {
    let dir = tempfile::tempdir().unwrap();
    let alice = MemoryManager::new(dir.path(), "telegram-alice");
    let bob = MemoryManager::new(dir.path(), "telegram-bob");
    day(&alice, "2026-03-01", &[("user", "my bank password hint")]);
    day(&bob, "2026-03-01", &[("user", "bob talks about lunch")]);

    assert!(bob
        .search_messages("bank password", 10, "", "", "")
        .is_empty());
    assert_eq!(
        alice.search_messages("bank password", 10, "", "", "").len(),
        1
    );
}

// ── facts ───────────────────────────────────────────────────────────────────

const FACTS: &str = "\
# Work
Ships the Nerv gateway.
Prefers Rust for the core.

# Travel
Based in Seattle.
Flies out of SEA.
";

fn facts_agent() -> AgentPrompt {
    AgentPrompt {
        name: "tester".to_string(),
        system_prompt: "p".to_string(),
        max_context_tokens: 8000,
    }
}

#[test]
fn fact_sections_split_on_headings() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    std::fs::write(&manager.facts_file, FACTS).unwrap();

    let sections = manager.fact_sections();
    let headings: Vec<&str> = sections.iter().map(|(h, _)| h.as_str()).collect();
    assert_eq!(headings, ["Work", "Travel"]);
    assert!(manager.read_fact_section("Work").contains("Prefers Rust"));
    // Heading lookup is case-insensitive.
    assert!(manager
        .read_fact_section("travel")
        .starts_with("Based in Seattle"));
    assert!(manager.read_fact_section("nope").is_empty());
}

#[test]
fn unstructured_facts_round_trip_as_preamble() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    std::fs::write(&manager.facts_file, "- likes tea\n- dislikes meetings\n").unwrap();

    let sections = manager.fact_sections();
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].0, "");
    assert!(sections[0].1.contains("likes tea"));
}

#[test]
fn outline_lists_titles_with_a_hint() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    std::fs::write(&manager.facts_file, FACTS).unwrap();

    let outline = manager.facts_outline();
    assert!(outline.contains("- Work: Ships the Nerv gateway."));
    assert!(outline.contains("- Travel: Based in Seattle."));
    // Each entry is a title plus a one-line hint, not the whole section.
    assert!(!outline.contains("Prefers Rust"));
    assert!(!outline.contains("Flies out of SEA"));
}

#[test]
fn facts_are_shared_across_scopes() {
    let dir = tempfile::tempdir().unwrap();
    let cli = manager(dir.path());
    let telegram = MemoryManager::new(dir.path(), "telegram-owner");

    cli.append_fact("Owner prefers metric units.");
    assert!(telegram.read_facts().contains("metric units"));
}

// ── context assembly ────────────────────────────────────────────────────────

#[test]
fn build_messages_limits_history_to_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    let context = ContextManager { memory: &manager };

    // Keep the context small enough to force truncation while still fitting
    // the base prompt, the current user message, and one short history entry.
    let mut agent = AgentPrompt {
        name: "Test".to_string(),
        system_prompt: "SYS".to_string(),
        max_context_tokens: 0,
    };
    let user_msg = "hello";
    let short_history = "short";
    let oversized_history = "huge history message that does not fit at all";
    let base_tokens = context_estimate_tokens(&agent.system_prompt);
    let lower_bound = (base_tokens
        + context_estimate_tokens(user_msg)
        + context_estimate_tokens(short_history)) as f64
        / 0.9;
    let upper_bound = (base_tokens
        + context_estimate_tokens(user_msg)
        + context_estimate_tokens(short_history)
        + context_estimate_tokens(oversized_history)) as f64
        / 0.9;
    agent.max_context_tokens = ((lower_bound + upper_bound) / 2.0) as i64;

    manager.append_message("user", oversized_history);
    manager.append_message("assistant", short_history);

    let messages = context.build_messages(&agent, user_msg, 10, &[]);
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].role, "system");
    assert!(messages[0].content.contains("SYS"));
    assert_eq!(messages[1].role, "assistant");
    assert_eq!(messages[1].content, "short");
    assert_eq!(messages[2].role, "user");
    assert_eq!(messages[2].content, "hello");
}

#[test]
fn facts_are_injected_into_the_system_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    manager.append_fact("User is a developer.");

    let context = ContextManager { memory: &manager };
    let agent = AgentPrompt {
        name: "Test".to_string(),
        system_prompt: "You are an AI.".to_string(),
        max_context_tokens: 8000,
    };

    let messages = context.build_messages(&agent, "hello", 10, &[]);
    assert!(messages[0].content.contains("User is a developer."));
    assert!(messages[0].content.contains("You are an AI."));
}

#[test]
fn the_persisted_user_message_is_not_duplicated_in_the_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    manager.append_message("user", "earlier");
    manager.append_message("assistant", "reply");
    manager.append_message("user", "hello now"); // persisted before build
    let agent = AgentPrompt {
        name: "A".to_string(),
        system_prompt: "s".to_string(),
        max_context_tokens: 8000,
    };

    let context = ContextManager { memory: &manager };
    let messages = context.build_messages(&agent, "hello now", 10, &[]);
    let roles: Vec<&str> = messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, ["system", "user", "assistant", "user"]);
    assert_eq!(
        messages.iter().filter(|m| m.content == "hello now").count(),
        1
    );
}

#[test]
fn extra_system_sections_are_appended() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    let context = ContextManager { memory: &manager };
    let agent = AgentPrompt {
        name: "General".to_string(),
        system_prompt: "You are an AI.".to_string(),
        max_context_tokens: 8000,
    };

    let messages = context.build_messages(
        &agent,
        "hello",
        10,
        &[
            "[Workflow Library]\n- Demo".to_string(),
            "[Skill Library]\n- Skill".to_string(),
        ],
    );

    let system_prompt = &messages[0].content;
    assert!(system_prompt.contains("[Workflow Library]"));
    assert!(system_prompt.contains("[Skill Library]"));
}

#[test]
fn small_facts_are_inlined() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    std::fs::write(&manager.facts_file, FACTS).unwrap();

    let context = ContextManager { memory: &manager };
    let system = context.build_messages(&facts_agent(), "hi", 10, &[])[0]
        .content
        .clone();
    assert!(system.contains("[Long-term Facts]"));
    assert!(system.contains("Prefers Rust"));
}

#[test]
fn large_facts_collapse_to_an_outline() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    let filler = "x ".repeat((FACTS_INLINE_TOKEN_BUDGET * 4) as usize);
    std::fs::write(
        &manager.facts_file,
        format!("# Work\n{filler}\n\n# Travel\nBased in Seattle.\n"),
    )
    .unwrap();

    let context = ContextManager { memory: &manager };
    let system = context.build_messages(&facts_agent(), "hi", 10, &[])[0]
        .content
        .clone();
    assert!(system.contains("[Long-term Facts — outline]"));
    assert!(system.contains("get_fact_section"));
    assert!(system.contains("- Work:") && system.contains("- Travel:"));
    assert!(!system.contains(filler.trim()));
}

#[test]
fn large_unstructured_facts_are_still_inlined() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    let body = "important detail ".repeat((FACTS_INLINE_TOKEN_BUDGET * 2) as usize);
    std::fs::write(&manager.facts_file, body).unwrap();

    let context = ContextManager { memory: &manager };
    let system = context.build_messages(&facts_agent(), "hi", 10, &[])[0]
        .content
        .clone();
    assert!(system.contains("[Long-term Facts]"));
    assert!(system.contains("important detail"));
}

#[test]
fn estimate_tokens_matches_language_ratios() {
    // Latin text is ~4 chars/token; CJK is ~1 token/char.
    let latin = "the quick brown fox jumps over the lazy dog"; // 43 chars
    assert!((10..=16).contains(&context_estimate_tokens(latin)));

    let cjk = "西雅图时间现在几点"; // 9 chars
    assert_eq!(context_estimate_tokens(cjk), 9);

    assert_eq!(context_estimate_tokens(""), 0);
}

// ── summaries ───────────────────────────────────────────────────────────────

fn yesterday() -> String {
    (Local::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string()
}

#[test]
fn summary_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    manager.write_summary("2026-03-01", "  Discussed the gateway rewrite.  ");
    assert_eq!(
        manager.read_summary("2026-03-01"),
        "Discussed the gateway rewrite."
    );
    assert_eq!(manager.read_summary("2026-03-02"), "");
}

#[test]
fn days_needing_summary_excludes_today_and_summarised() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    let today = MemoryManager::today_str();
    day(&manager, &today, &[("user", "still talking")]);
    day(&manager, "2026-03-01", &[("user", "old talk")]);
    day(&manager, "2026-03-02", &[("user", "other old talk")]);
    manager.write_summary("2026-03-02", "already done");

    let pending = manager.days_needing_summary(5);
    assert_eq!(pending, ["2026-03-01"]);
}

#[test]
fn long_day_keeps_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    let long = "x".repeat(MAX_CHARS_PER_DAY);
    day(
        &manager,
        "2026-03-01",
        &[("user", long.as_str()), ("user", "the decision")],
    );

    let body = summarizer::render_day(&manager, "2026-03-01");
    assert!(body.contains("the decision"));
    assert!(body.starts_with("...[earlier turns omitted]"));
}

#[test]
fn summaries_reach_the_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    day(
        &manager,
        &yesterday(),
        &[("user", "the telegram adapter drops long replies")],
    );
    manager.write_summary(&yesterday(), "Found the telegram adapter truncation bug.");

    let context = ContextManager { memory: &manager };
    let system = context.build_messages(&facts_agent(), "hi", 10, &[])[0]
        .content
        .clone();
    assert!(system.contains("[Earlier days — summaries]"));
    assert!(system.contains("telegram adapter truncation"));
}

#[test]
fn summary_injection_respects_its_budget() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    for day_num in 1..4 {
        let date = (Local::now() - chrono::Duration::days(day_num))
            .format("%Y-%m-%d")
            .to_string();
        day(&manager, &date, &[("user", "talk")]);
        manager.write_summary(&date, &"word ".repeat((SUMMARY_TOKEN_BUDGET * 2) as usize));
    }

    let context = ContextManager { memory: &manager };
    let system = context.build_messages(&facts_agent(), "hi", 10, &[])[0]
        .content
        .clone();
    // One oversized summary alone blows the budget, so at most one gets in.
    assert!(system.matches("- 20").count() <= 1);
}

#[test]
fn no_summaries_adds_no_section() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    day(&manager, "2026-03-01", &[("user", "talk")]);

    let context = ContextManager { memory: &manager };
    let system = context.build_messages(&facts_agent(), "hi", 10, &[])[0]
        .content
        .clone();
    assert!(!system.contains("Earlier days"));
}

// ── summarizer (scripted model caller) ──────────────────────────────────────

/// A scripted caller: pops one prepared outcome per call. Mirrors the python
/// tests' monkeypatched `nerv.llm.complete`.
struct ScriptedCaller {
    outcomes: Mutex<VecDeque<Result<String, String>>>,
    prompts: Mutex<Vec<String>>,
    models: Mutex<Vec<String>>,
}

impl ScriptedCaller {
    fn new(outcomes: Vec<Result<String, String>>) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into()),
            prompts: Mutex::new(Vec::new()),
            models: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl ModelCaller for ScriptedCaller {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        model: &str,
        _tools: Option<&[serde_json::Value]>,
        _tool_names: &[String],
        _scope: &str,
    ) -> Result<CompletionResult, LlmError> {
        self.prompts.lock().unwrap().push(
            messages
                .first()
                .map(|m| m.content.clone())
                .unwrap_or_default(),
        );
        self.models.lock().unwrap().push(model.to_string());
        match self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err("model down".to_string()))
        {
            Ok(content) => Ok(CompletionResult {
                content,
                tool_calls: Vec::new(),
                runs_own_tool_loop: false,
                provider: String::new(),
                duration_ms: None,
                cost_usd: None,
                usage: None,
            }),
            Err(message) => Err(LlmError::Unsupported(message)),
        }
    }
}

#[tokio::test]
async fn summarize_day_stores_model_output() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    day(
        &manager,
        "2026-03-01",
        &[
            ("user", "we picked vite 8"),
            ("user", "and dropped esbuild"),
        ],
    );

    let caller = ScriptedCaller::new(vec![Ok("Chose vite 8 and dropped esbuild.".to_string())]);
    let out = summarizer::summarize_day(&manager, "2026-03-01", &caller, "local:test", None).await;

    assert_eq!(out, "Chose vite 8 and dropped esbuild.");
    assert_eq!(manager.read_summary("2026-03-01"), out);
    // The day's turns reached the prompt.
    let prompts = caller.prompts.lock().unwrap();
    assert!(prompts[0].contains("we picked vite 8"));
    let models = caller.models.lock().unwrap();
    assert_eq!(models[0], "local:test");
}

#[tokio::test]
async fn summarize_day_survives_model_failure() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    day(&manager, "2026-03-01", &[("user", "something")]);

    let caller = ScriptedCaller::new(vec![Err("model down".to_string())]);
    let out = summarizer::summarize_day(&manager, "2026-03-01", &caller, "x", None).await;
    assert_eq!(out, "");
    assert_eq!(manager.read_summary("2026-03-01"), "");
}

#[tokio::test]
async fn backfill_is_bounded_per_call() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    for day_num in 1..6 {
        day(
            &manager,
            &format!("2026-03-0{day_num}"),
            &[("user", "talk")],
        );
    }

    let caller = ScriptedCaller::new(vec![Ok("a summary".to_string()); 10]);
    let written = summarizer::backfill_summaries(&manager, &caller, "x", None, 2).await;
    assert_eq!(written, 2);
    assert_eq!(caller.prompts.lock().unwrap().len(), 2);
    // The rest stay pending for later turns.
    assert_eq!(manager.days_needing_summary(10).len(), 3);
}

#[tokio::test]
async fn concurrent_backfills_summarise_each_day_once() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(dir.path());
    day(&manager, "2020-03-01", &[("user", "talk one")]);
    day(&manager, "2020-03-02", &[("user", "talk two")]);

    // Three overlapping runs: the in-flight set must dedupe per day.
    let caller = Arc::new(ScriptedCaller::new(vec![Ok("a summary".to_string()); 4]));
    let (a, b, c) = (
        summarizer::backfill_summaries(&manager, caller.as_ref(), "x", None, 2),
        summarizer::backfill_summaries(&manager, caller.as_ref(), "x", None, 2),
        summarizer::backfill_summaries(&manager, caller.as_ref(), "x", None, 2),
    );
    let (a, b, c) = tokio::join!(a, b, c);
    assert_eq!(a + b + c, 2);
    assert_eq!(caller.prompts.lock().unwrap().len(), 2);
    // A later run finds nothing left to do.
    assert_eq!(
        summarizer::backfill_summaries(&manager, caller.as_ref(), "x", None, 2).await,
        0
    );
    assert_eq!(caller.prompts.lock().unwrap().len(), 2);
}
