//! Context Manager — constructs the prompt array for the model turn, handling
//! token limits. Port of `brain/nerv/memory/context.py`.

use chrono::Local;

use super::manager::MemoryManager;
use crate::llm::ChatMessage;

/// Token estimation heuristic, chosen to avoid a heavy tokenizer dependency.
/// CJK characters are roughly one token each; Latin text is roughly 4
/// characters per token, so we divide by a slightly conservative 3.5 to stay
/// under the real count.
const CJK_RANGES: &[(u32, u32)] = &[
    (0x3040, 0x30ff), // Hiragana + Katakana
    (0x3400, 0x4dbf), // CJK Extension A
    (0x4e00, 0x9fff), // CJK Unified Ideographs
    (0xac00, 0xd7af), // Hangul syllables
    (0xf900, 0xfaff), // CJK Compatibility Ideographs
];

const LATIN_CHARS_PER_TOKEN: f64 = 3.5;

/// Above this, `facts.md` stops being pasted in full and the prompt carries
/// only a section outline; an agent pulls what it needs with `get_fact_section`.
pub const FACTS_INLINE_TOKEN_BUDGET: i64 = 400;

/// How many summarised earlier days to carry, and the ceiling on what they
/// may spend. Summaries are the only trace of conversation older than the
/// recency window, so they get a reserved slice rather than competing with
/// raw history.
pub(crate) const MAX_SUMMARY_DAYS: usize = 3;
pub(crate) const SUMMARY_TOKEN_BUDGET: i64 = 500;

fn is_cjk(c: char) -> bool {
    let cp = c as u32;
    CJK_RANGES
        .iter()
        .any(|&(low, high)| (low..=high).contains(&cp))
}

/// Rough token estimate — a safe upper bound without importing tiktoken.
pub fn estimate_tokens(text: &str) -> i64 {
    if text.is_empty() {
        return 0;
    }
    let cjk = text.chars().filter(|c| is_cjk(*c)).count();
    let latin = text.chars().count() - cjk;
    let latin_tokens = (latin as f64 / LATIN_CHARS_PER_TOKEN).ceil() as i64;
    cjk as i64 + latin_tokens
}

/// The agent-facing inputs for one prompt build (the python side no longer
/// assembles context — it hands these over in `prepare_turn`).
pub struct AgentPrompt {
    pub name: String,
    pub system_prompt: String,
    pub max_context_tokens: i64,
}

/// Builds prompt windows for the orchestrator, ensuring they fit context
/// limits.
pub struct ContextManager<'a> {
    pub memory: &'a MemoryManager,
}

impl ContextManager<'_> {
    /// Inline the facts file, or just its outline once it gets expensive.
    fn render_facts(&self, facts: &str) -> String {
        if estimate_tokens(facts) <= FACTS_INLINE_TOKEN_BUDGET {
            return format!("[Long-term Facts]\n{}", facts.trim());
        }

        let headed = self
            .memory
            .fact_sections()
            .iter()
            .any(|(h, _)| !h.is_empty());
        let outline = if headed {
            self.memory.facts_outline()
        } else {
            String::new()
        };
        if outline.is_empty() {
            // Big but unstructured: no headings to summarise, so inline it and
            // let the history budget absorb the cost rather than drop facts.
            return format!("[Long-term Facts]\n{}", facts.trim());
        }

        tracing::debug!("Facts exceed inline budget; sending outline only");
        format!(
            "[Long-term Facts — outline]\n\
             Only section titles are shown. Call `get_fact_section` with a \
             title to read one in full.\n{outline}"
        )
    }

    /// Render recent days' rolling summaries, newest first, within budget.
    fn render_summaries(&self) -> String {
        let today = Local::now().format("%Y-%m-%d").to_string();
        let entries = self.memory.summaries_before(&today, MAX_SUMMARY_DAYS);
        if entries.is_empty() {
            return String::new();
        }

        let mut lines: Vec<String> = Vec::new();
        let mut spent = 0;
        for (day, summary) in entries {
            let cost = estimate_tokens(&summary);
            if spent + cost > SUMMARY_TOKEN_BUDGET {
                break;
            }
            lines.push(format!("- {day}: {summary}"));
            spent += cost;
        }

        if lines.is_empty() {
            return String::new();
        }
        format!("[Earlier days — summaries]\n{}", lines.join("\n"))
    }

    /// Construct the list of messages for the LLM invocation.
    ///
    /// Rules:
    /// 1. Base System prompt (Agent's definition).
    /// 2. Extra sections (workflow/skill catalogs).
    /// 3. Long-term facts.
    /// 4. Summarised earlier days.
    /// 5. Recent history (within budget).
    /// 6. Current user message.
    ///
    /// The caller persists the user's message before building the prompt; the
    /// trailing copy is dropped so it is not sent twice (same as python).
    pub fn build_messages(
        &self,
        agent: &AgentPrompt,
        user_msg: &str,
        max_history_msg: usize,
        extra_system_sections: &[String],
    ) -> Vec<ChatMessage> {
        let facts = self.memory.read_facts();
        let mut system_content = agent.system_prompt.clone();
        for section in extra_system_sections {
            if !section.trim().is_empty() {
                system_content.push_str("\n\n");
                system_content.push_str(section.trim());
            }
        }
        if !facts.trim().is_empty() {
            system_content.push_str("\n\n");
            system_content.push_str(&self.render_facts(&facts));
        }

        let summaries = self.render_summaries();
        if !summaries.is_empty() {
            system_content.push_str("\n\n");
            system_content.push_str(&summaries);
        }

        // 1. System message is always included.
        let mut messages = vec![ChatMessage::system(system_content.clone())];
        let mut tokens_used = estimate_tokens(&system_content);

        // 2. Add current user message (we MUST fit this).
        let current_msg_tokens = estimate_tokens(user_msg);
        if current_msg_tokens > (agent.max_context_tokens as f64 * 0.5) as i64 {
            tracing::warn!("User message is extremely large, might truncate history.");
        }

        // 3. Retrieve history and append if it fits.
        let mut history = self.memory.load_recent_messages(max_history_msg + 1);
        // Drop the trailing copy of the just-persisted user message so it is
        // not sent twice.
        if let Some(last) = history.last() {
            if last.role == "user" && last.content == user_msg {
                history.pop();
            }
        }
        let skip = if max_history_msg > 0 {
            history.len().saturating_sub(max_history_msg)
        } else {
            history.len()
        };
        let history = &history[skip..];

        // Walk backwards: we want the most recent bits of history.
        let mut history_msgs: Vec<ChatMessage> = Vec::new();
        for msg in history.iter().rev() {
            let msg_tokens = estimate_tokens(&msg.content);
            // Safe boundary check: reserve space for the current message + 10%
            // buffer.
            let available_tokens = (agent.max_context_tokens as f64
                - tokens_used as f64
                - current_msg_tokens as f64
                - agent.max_context_tokens as f64 * 0.1) as i64;
            if available_tokens > msg_tokens {
                history_msgs.insert(0, ChatMessage::new(&msg.role, &msg.content));
                tokens_used += msg_tokens;
            } else {
                tracing::debug!("Truncating history at message length {}", msg.content.len());
                break;
            }
        }
        messages.extend(history_msgs);
        messages.push(ChatMessage::user(user_msg));

        messages
    }
}
