"""Rolling summaries for past conversation days.

CLAUDE.md forbids sending full history to the model, and the context manager
only ever carries the last handful of messages. That leaves a gap: a day that
scrolled out of the window becomes invisible unless a fact was promoted from
it. Summaries close the gap cheaply — one short paragraph per day, generated
once, injected as context afterwards.

Only *completed* days are summarised. Today is still being appended to, so
summarising it would be redone on the next turn.
"""

from __future__ import annotations

import logging
from pathlib import Path

from nerv.memory.manager import MemoryManager

logger = logging.getLogger(__name__)

# Enough of a day to summarise faithfully without paying for a huge prompt.
MAX_CHARS_PER_DAY = 12000

_PROMPT = """\
Summarise this day of conversation between a user and their assistant.

Write 2-4 sentences capturing what was decided, what the user asked for, and \
any durable detail worth recalling weeks later (names, versions, paths, \
preferences). Omit pleasantries. Write plain prose, no bullet points, no \
preamble — output only the summary itself.

Conversation:
{body}"""


def _render_day(memory: MemoryManager, date_str: str) -> str:
    """Flatten one day's messages into prompt text, oldest first."""
    conv = memory.load_conversation(date_str)
    lines = [f"{m.role}: {' '.join(m.content.split())}" for m in conv.messages]
    body = "\n".join(lines)
    if len(body) > MAX_CHARS_PER_DAY:
        # Keep the end of the day: conclusions matter more than opening chatter.
        body = "...[earlier turns omitted]\n" + body[-MAX_CHARS_PER_DAY:]
    return body


async def summarize_day(
    memory: MemoryManager,
    date_str: str,
    *,
    model: str,
    project_root: Path | None = None,
) -> str:
    """Generate and store the summary for one day. Returns it, or "" on failure."""
    from nerv.llm import complete

    body = _render_day(memory, date_str)
    if not body.strip():
        return ""

    try:
        result = await complete(
            [{"role": "user", "content": _PROMPT.format(body=body)}],
            model=model,
            project_root=project_root,
            temperature=0.0,
        )
    except Exception as e:
        # A failed summary must never break the turn that triggered it.
        logger.warning("Failed to summarise %s: %s", date_str, e)
        return ""

    summary = result.content.strip()
    if not summary:
        return ""

    memory.write_summary(date_str, summary)
    logger.info("Summarised conversation day %s (%d chars)", date_str, len(summary))
    return summary


async def backfill_summaries(
    memory: MemoryManager,
    *,
    model: str,
    project_root: Path | None = None,
    limit: int = 2,
) -> int:
    """Summarise up to ``limit`` past days that do not have a summary yet.

    Returns how many were written. Bounded per call so a long-dormant install
    catches up over several turns instead of stalling one on a burst of
    model calls.
    """
    written = 0
    for date_str in memory.days_needing_summary(limit=limit):
        if await summarize_day(
            memory, date_str, model=model, project_root=project_root
        ):
            written += 1
    return written
