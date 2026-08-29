"""Context Manager constructs prompt arrays for Ollama, handling token limits."""

import logging
import math
from typing import Any

from nerv.memory.manager import MemoryManager
from nerv.orchestrator.registry import AgentDefinition
from nerv.orchestrator.runtime_policy import build_system_prompt

logger = logging.getLogger(__name__)

# Token estimation heuristic, chosen to avoid a heavy tokenizer dependency.
# CJK characters are roughly one token each; Latin text is roughly 4 characters
# per token, so we divide by a slightly conservative 3.5 to stay under the real
# count. Treating every character as one token (the previous rule) overestimated
# Latin text ~4x and silently shrank the usable history window.
_CJK_RANGES: tuple[tuple[str, str], ...] = (
    ("\u3040", "\u30ff"),  # Hiragana + Katakana
    ("\u3400", "\u4dbf"),  # CJK Extension A
    ("\u4e00", "\u9fff"),  # CJK Unified Ideographs
    ("\uac00", "\ud7af"),  # Hangul syllables
    ("\uf900", "\ufaff"),  # CJK Compatibility Ideographs
)

_LATIN_CHARS_PER_TOKEN = 3.5

# Above this, `facts.md` stops being pasted in full and the prompt carries only
# a section outline; an agent pulls what it needs with `get_fact_section`.
# Below it, inlining is cheaper than making the model spend a tool call.
FACTS_INLINE_TOKEN_BUDGET = 400

# How many summarised earlier days to carry, and the ceiling on what they may
# spend. Summaries are the only trace of conversation older than the recency
# window, so they get a reserved slice rather than competing with raw history.
MAX_SUMMARY_DAYS = 3
SUMMARY_TOKEN_BUDGET = 500


def _is_cjk(char: str) -> bool:
    return any(low <= char <= high for low, high in _CJK_RANGES)


def estimate_tokens(text: str) -> int:
    """Rough token estimate — a safe upper bound without importing tiktoken."""
    if not text:
        return 0
    cjk = sum(1 for char in text if _is_cjk(char))
    latin = len(text) - cjk
    return cjk + math.ceil(latin / _LATIN_CHARS_PER_TOKEN)


class ContextManager:
    """Builds prompt windows for the orchestrator, ensuring it fits context limits."""

    def __init__(self, memory: MemoryManager) -> None:
        self.memory = memory

    def _render_facts(self, facts: str) -> str:
        """Inline the facts file, or just its outline once it gets expensive."""
        if estimate_tokens(facts) <= FACTS_INLINE_TOKEN_BUDGET:
            return f"[Long-term Facts]\n{facts.strip()}"

        headed = any(heading for heading, _ in self.memory.fact_sections())
        outline = self.memory.facts_outline() if headed else ""
        if not outline:
            # Big but unstructured: no headings to summarise, so inline it and
            # let the history budget absorb the cost rather than drop facts.
            return f"[Long-term Facts]\n{facts.strip()}"

        logger.debug("Facts exceed inline budget; sending outline only")
        return (
            "[Long-term Facts — outline]\n"
            "Only section titles are shown. Call `get_fact_section` with a "
            "title to read one in full.\n"
            f"{outline}"
        )

    def _render_summaries(self) -> str:
        """Render recent days' rolling summaries, newest first, within budget."""
        from datetime import datetime

        today = datetime.now().strftime("%Y-%m-%d")
        entries = self.memory.summaries_before(today, limit=MAX_SUMMARY_DAYS)
        if not entries:
            return ""

        lines: list[str] = []
        spent = 0
        for day, summary in entries:
            cost = estimate_tokens(summary)
            if spent + cost > SUMMARY_TOKEN_BUDGET:
                break
            lines.append(f"- {day}: {summary}")
            spent += cost

        if not lines:
            return ""
        return "[Earlier days — summaries]\n" + "\n".join(lines)

    def build_messages(
        self,
        agent: AgentDefinition,
        user_msg: str,
        max_history_msg: int = 10,
        extra_system_sections: list[str] | None = None,
    ) -> list[dict[str, Any]]:
        """Construct the list of messages for the LLM invocation.

        Rules:
        1. Base System prompt (Agent's definition).
        2. Long-term facts.
        3. Recent messages (history).
        4. Current user message.

        Must ensure total tokens <= agent.max_context_tokens.
        """
        # Load facts
        facts = self.memory.read_facts()
        system_content = build_system_prompt(agent)
        if extra_system_sections:
            for section in extra_system_sections:
                if section.strip():
                    system_content += f"\n\n{section.strip()}"
        if facts.strip():
            system_content += f"\n\n{self._render_facts(facts)}"

        summaries = self._render_summaries()
        if summaries:
            system_content += f"\n\n{summaries}"

        # 1. System message is always included
        messages: list[dict[str, Any]] = [{"role": "system", "content": system_content}]
        tokens_used = estimate_tokens(system_content)

        # 2. Add current user message (we MUST fit this)
        current_msg_tokens = estimate_tokens(user_msg)
        if current_msg_tokens > agent.max_context_tokens * 0.5:
            logger.warning("User message is extremely large, might truncate history.")

        # 3. Retrieve history and append if it fits
        history = self.memory.load_recent_messages(limit=max_history_msg)

        history_msgs_to_add: list[dict[str, Any]] = []
        # Walk backwards: we want the most recent bits of history
        for msg in reversed(history):
            msg_tokens = estimate_tokens(msg.content)
            # Safe boundary check: we reserve space for current message + 10% buffer
            available_tokens = (
                agent.max_context_tokens
                - tokens_used
                - current_msg_tokens
                - (agent.max_context_tokens * 0.1)
            )
            if available_tokens > msg_tokens:
                history_msgs_to_add.insert(
                    0, {"role": msg.role, "content": msg.content}
                )
                tokens_used += msg_tokens
            else:
                # Out of space for older history
                logger.debug(
                    "Truncating history at message length %d", len(msg.content)
                )
                break

        messages.extend(history_msgs_to_add)
        messages.append({"role": "user", "content": user_msg})

        return messages
