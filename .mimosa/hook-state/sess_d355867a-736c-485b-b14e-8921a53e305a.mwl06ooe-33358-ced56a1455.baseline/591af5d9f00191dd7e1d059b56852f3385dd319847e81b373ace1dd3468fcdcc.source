"""Memory tools: searching past conversations and reading long-term facts.

The orchestrator only ever puts the *most recent* messages in the prompt, so
without these tools anything older than the history window is unreachable
unless it was promoted into ``facts.md``. These let an agent go looking.

Retrieval is scoped: a tool call reads the conversation scope the current
request belongs to (see :func:`nerv.memory.manager.active_scope`) and never
another sender's history.
"""

import logging

from nerv.memory.manager import MemoryManager, active_scope
from nerv.tools._helpers import _get_project_root, _truncate
from nerv.tools.registry import ToolResult, registry

logger = logging.getLogger(__name__)

# Enough of a message to judge relevance without flooding the context window.
_SNIPPET_CHARS = 400


def _memory() -> MemoryManager:
    """Build a MemoryManager bound to the current request's scope."""
    return MemoryManager(_get_project_root(), active_scope())


@registry.register(
    name="search_memory",
    description=(
        "Search this conversation's own past messages by keyword. Use it to "
        "recall something older than the recent history already in context — "
        "a decision, a name, a link mentioned days or weeks ago. Optionally "
        "bound by date (YYYY-MM-DD) or restrict to one speaker."
    ),
    requires_confirmation=False,
)
def search_memory(
    query: str,
    limit: int = 10,
    since: str = "",
    until: str = "",
    role: str = "",
) -> ToolResult:
    """Search past conversation messages in the active scope."""
    if not query.strip():
        return ToolResult(content="Error: query must not be empty.", is_error=True)

    limit = max(1, min(limit, 50))
    try:
        hits = _memory().search_messages(
            query, limit=limit, since=since, until=until, role=role
        )
    except OSError as e:
        logger.error("search_memory failed: %s", e)
        return ToolResult(content=f"Error searching memory: {e}", is_error=True)

    if not hits:
        return ToolResult(content=f"No past messages match {query!r}.")

    lines = [f"{len(hits)} match(es) for {query!r}, best first:", ""]
    for hit in hits:
        stamp = hit.message.timestamp.strftime("%H:%M")
        snippet = " ".join(hit.message.content.split())
        if len(snippet) > _SNIPPET_CHARS:
            snippet = snippet[:_SNIPPET_CHARS] + "…"
        lines.append(f"[{hit.date} {stamp}] {hit.message.role}: {snippet}")

    return ToolResult(content=_truncate("\n".join(lines)))


@registry.register(
    name="get_fact_section",
    description=(
        "Read one section of the long-term facts file in full. The system "
        "prompt lists only section titles once the file grows large; use this "
        "to pull back the one you need. Call with no title to list them."
    ),
    requires_confirmation=False,
)
def get_fact_section(title: str = "") -> ToolResult:
    """Return one titled section of ``facts.md``, or the outline if untitled."""
    memory = _memory()
    if not title.strip():
        outline = memory.facts_outline()
        if not outline:
            return ToolResult(content="No long-term facts are recorded yet.")
        return ToolResult(content=f"Fact sections:\n{outline}")

    body = memory.read_fact_section(title)
    if not body:
        outline = memory.facts_outline()
        return ToolResult(
            content=(
                f"No fact section titled {title!r}."
                + (f" Available:\n{outline}" if outline else "")
            ),
            is_error=True,
        )
    return ToolResult(content=_truncate(f"## {title.strip()}\n{body}"))
