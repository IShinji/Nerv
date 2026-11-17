"""Everyday productivity tools: local calendar (ICS), notes (Markdown), email.

Per the parsimony rule, local data lives in native tools (calendar → a local
ICS file, notes → Markdown under ``personal/notes/``). Email is an external
service, so it is MCP-first: it routes through a configured email MCP server and
degrades gracefully with setup guidance when none is configured.
"""

from __future__ import annotations

import datetime as dt
import logging
import re

from nerv.tools.builtins import _get_project_root
from nerv.tools.registry import ToolResult, registry

logger = logging.getLogger(__name__)


def _calendar_path():
    path = _get_project_root() / "personal" / "calendar.ics"
    path.parent.mkdir(parents=True, exist_ok=True)
    return path


def _notes_dir():
    path = _get_project_root() / "personal" / "notes"
    path.mkdir(parents=True, exist_ok=True)
    return path


def _slugify(title: str) -> str:
    slug = re.sub(r"[^a-z0-9]+", "-", title.strip().lower()).strip("-")
    return slug or "note"


def _to_ics_dt(value: str) -> str:
    """Normalize an ISO-ish datetime to an ICS timestamp (best effort)."""
    value = value.strip()
    try:
        parsed = dt.datetime.fromisoformat(value)
        return parsed.strftime("%Y%m%dT%H%M%S")
    except ValueError:
        return re.sub(r"[^0-9T]", "", value) or value


@registry.register(
    name="calendar_add",
    description="Add an event to the local calendar. start/end are ISO datetimes "
    "like '2026-06-01T09:00'. Returns confirmation.",
    requires_confirmation=False,
)
def calendar_add(title: str, start: str, end: str = "", notes: str = "") -> ToolResult:
    """Append a VEVENT to the local ICS calendar."""
    path = _calendar_path()
    uid = f"{dt.datetime.now().strftime('%Y%m%dT%H%M%S')}-{_slugify(title)}@nerv"
    dtstart = _to_ics_dt(start)
    dtend = _to_ics_dt(end) if end else dtstart

    lines = [
        "BEGIN:VEVENT",
        f"UID:{uid}",
        f"DTSTAMP:{dt.datetime.now().strftime('%Y%m%dT%H%M%S')}",
        f"DTSTART:{dtstart}",
        f"DTEND:{dtend}",
        f"SUMMARY:{title}",
    ]
    if notes:
        lines.append(f"DESCRIPTION:{notes}")
    lines.append("END:VEVENT")
    block = "\n".join(lines) + "\n"

    if not path.exists():
        header = "BEGIN:VCALENDAR\nVERSION:2.0\nPRODID:-//Nerv//EN\n"
        path.write_text(header + block + "END:VCALENDAR\n", encoding="utf-8")
    else:
        text = path.read_text(encoding="utf-8")
        if "END:VCALENDAR" in text:
            text = text.replace("END:VCALENDAR", block + "END:VCALENDAR")
        else:
            text = text + block
        path.write_text(text, encoding="utf-8")

    return ToolResult(content=f"Added calendar event '{title}' at {start}.")


@registry.register(
    name="calendar_list",
    description="List upcoming events from the local calendar.",
    requires_confirmation=False,
)
def calendar_list(limit: int = 20) -> ToolResult:
    """List events (SUMMARY @ DTSTART) from the local ICS calendar."""
    path = _calendar_path()
    if not path.exists():
        return ToolResult(content="The calendar is empty.")

    events: list[tuple[str, str]] = []
    summary = start = ""
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("SUMMARY:"):
            summary = line[len("SUMMARY:") :]
        elif line.startswith("DTSTART:"):
            start = line[len("DTSTART:") :]
        elif line.startswith("END:VEVENT"):
            if summary:
                events.append((start, summary))
            summary = start = ""

    if not events:
        return ToolResult(content="The calendar is empty.")

    events.sort()
    lines = [f"- {start}: {summary}" for start, summary in events[:limit]]
    return ToolResult(content="Upcoming events:\n" + "\n".join(lines))


@registry.register(
    name="note_write",
    description="Create or overwrite a Markdown note by title in personal/notes.",
    requires_confirmation=False,
)
def note_write(title: str, content: str) -> ToolResult:
    """Write a Markdown note."""
    path = _notes_dir() / f"{_slugify(title)}.md"
    path.write_text(f"# {title}\n\n{content}\n", encoding="utf-8")
    return ToolResult(content=f"Saved note '{title}'.")


@registry.register(
    name="note_list",
    description="List saved notes by title.",
    requires_confirmation=False,
)
def note_list() -> ToolResult:
    """List saved notes by their human title (the first Markdown heading)."""
    titles: list[str] = []
    for path in sorted(_notes_dir().glob("*.md")):
        first_line = path.read_text(encoding="utf-8").splitlines()[:1]
        title = first_line[0].lstrip("# ").strip() if first_line else path.stem
        titles.append(title or path.stem)
    if not titles:
        return ToolResult(content="No notes saved yet.")
    return ToolResult(content="Notes:\n" + "\n".join(f"- {t}" for t in sorted(titles)))


@registry.register(
    name="note_read",
    description="Read the contents of a saved note by title.",
    requires_confirmation=False,
)
def note_read(title: str) -> ToolResult:
    """Read a Markdown note."""
    path = _notes_dir() / f"{_slugify(title)}.md"
    if not path.exists():
        return ToolResult(content=f"No note titled '{title}'.", is_error=True)
    return ToolResult(content=path.read_text(encoding="utf-8"))


@registry.register(
    name="send_email",
    description="Send an email. Requires a configured email MCP server.",
    requires_confirmation=True,
)
async def send_email(to: str, subject: str, body: str) -> ToolResult:
    """Send email via a configured MCP server (MCP-first, graceful fallback)."""
    from nerv.mcp.manager import get_mcp_manager

    manager = await get_mcp_manager(_get_project_root())
    if not manager.has_capability_binding("email.send"):
        return ToolResult(
            content=(
                "No email provider is configured. Add an email MCP server under "
                "`mcp.servers` in config (bound to capability `email.send`) to "
                "enable sending. Drafted message was not sent."
            ),
            is_error=True,
        )

    result = await manager.invoke_capability_action(
        "email.send", "send", {"to": to, "subject": subject, "body": body}
    )
    if result is None:
        return ToolResult(
            content="The configured email server could not handle the send action.",
            is_error=True,
        )
    return result
