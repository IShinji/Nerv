"""Manager for long-term and short-term memory persistence.

Conversations are stored per *scope* so two people talking to the same Nerv
instance never read each other's history:

    personal/memory/conversations/YYYY-MM-DD.jsonl          (default scope)
    personal/memory/scopes/<scope>/conversations/...jsonl   (named scope)

A scope is derived from the originating channel and sender (see
:func:`scope_for`). The default scope keeps the historical paths so an
existing install keeps its conversations.

Each conversation file is JSON Lines — one :class:`ChatMessage` per line — so
appending a turn is an O(1) append instead of an O(n) rewrite of the whole
day. Legacy ``YYYY-MM-DD.json`` files (a single ``DailyConversation`` object)
are still read for backwards compatibility.

Long-term ``facts.md`` is deliberately *not* scoped: it holds what Nerv knows
about its owner, and that should follow the owner across channels. Only
allow-listed senders can reach the gateway at all (see the ``allowed_users``
channel config), so facts stay inside the trusted circle.

Writes take an exclusive cross-process file lock, because the MCP server runs
in a separate process from the orchestrator and both can append.
"""

from __future__ import annotations

import contextlib
import fcntl
import json
import logging
import os
import re
import tempfile
from collections.abc import Iterator
from datetime import datetime
from pathlib import Path

from nerv.memory.models import ChatMessage, DailyConversation

logger = logging.getLogger(__name__)

DEFAULT_SCOPE = "default"

_UNSAFE_SCOPE_CHARS = re.compile(r"[^a-z0-9_.-]+")


def scope_for(channel: str = "", sender: str = "") -> str:
    """Build a filesystem-safe conversation scope from channel and sender.

    Messages with neither a channel nor a sender (internal calls, tests) map to
    :data:`DEFAULT_SCOPE`, which keeps the historical on-disk layout.
    """
    parts = [
        part.strip().lower() for part in (channel, sender) if part and part.strip()
    ]
    if not parts:
        return DEFAULT_SCOPE
    slug = _UNSAFE_SCOPE_CHARS.sub("-", "-".join(parts)).strip("-")
    return slug or DEFAULT_SCOPE


class MemoryManager:
    """Manages persistence of conversation and facts for one scope."""

    def __init__(self, project_root: Path, scope: str = DEFAULT_SCOPE) -> None:
        self.scope = scope or DEFAULT_SCOPE
        self.memory_dir = project_root / "personal" / "memory"
        if self.scope == DEFAULT_SCOPE:
            self.conv_dir = self.memory_dir / "conversations"
        else:
            self.conv_dir = self.memory_dir / "scopes" / self.scope / "conversations"
        # Facts are shared across scopes on purpose — see the module docstring.
        self.facts_file = self.memory_dir / "facts.md"
        self._lock_path = self.conv_dir / ".lock"
        # Facts are shared, so they need a lock that is shared too.
        self._facts_lock_path = self.memory_dir / ".facts.lock"
        self._ensure_directories()

    def _ensure_directories(self) -> None:
        """Create necessary memory directories if they don't exist."""
        self.conv_dir.mkdir(parents=True, exist_ok=True)
        if not self.facts_file.exists():
            self.facts_file.parent.mkdir(parents=True, exist_ok=True)
            self.facts_file.touch()

    # ── Locking ──────────────────────────────────────────────────────────────

    @staticmethod
    @contextlib.contextmanager
    def _flock(lock_path: Path) -> Iterator[None]:
        """Hold an exclusive cross-process lock on ``lock_path``."""
        lock_path.parent.mkdir(parents=True, exist_ok=True)
        with open(lock_path, "w") as lock_file:
            fcntl.flock(lock_file, fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(lock_file, fcntl.LOCK_UN)

    def _locked(self) -> contextlib.AbstractContextManager[None]:
        """Lock this scope's conversation directory."""
        return self._flock(self._lock_path)

    def _locked_facts(self) -> contextlib.AbstractContextManager[None]:
        """Lock the shared facts file (all scopes contend for this one)."""
        return self._flock(self._facts_lock_path)

    # ── Paths ────────────────────────────────────────────────────────────────

    def _get_today_str(self) -> str:
        """Get current date as YYYY-MM-DD."""
        return datetime.now().strftime("%Y-%m-%d")

    def _get_daily_file(self, date_str: str) -> Path:
        """Get the JSON Lines file path for a specific day."""
        return self.conv_dir / f"{date_str}.jsonl"

    def _legacy_daily_file(self, date_str: str) -> Path:
        """Get the pre-JSONL file path for a specific day."""
        return self.conv_dir / f"{date_str}.json"

    def _summary_file(self, date_str: str) -> Path:
        """Sidecar holding the day's rolling summary, when one exists."""
        return self.conv_dir / f"{date_str}.summary.md"

    def _dated_files(self) -> list[tuple[str, Path]]:
        """Return ``(date_str, path)`` for every stored day, oldest first."""
        found: dict[str, Path] = {}
        for path in self.conv_dir.glob("*.json"):
            found.setdefault(path.stem, path)
        # JSONL wins over a same-day legacy file.
        for path in self.conv_dir.glob("*.jsonl"):
            found[path.stem] = path
        return sorted(found.items())

    # ── Reading ──────────────────────────────────────────────────────────────

    def _read_messages(self, path: Path) -> list[ChatMessage]:
        """Read one day's messages from a JSONL or legacy JSON file."""
        try:
            raw = path.read_text(encoding="utf-8")
        except OSError as e:
            logger.error("Failed to read conversation %s: %s", path, e)
            return []

        if path.suffix == ".json":
            try:
                data = json.loads(raw)
            except json.JSONDecodeError as e:
                logger.error("Failed to parse conversation %s: %s", path, e)
                return []
            return DailyConversation.model_validate(data).messages

        messages: list[ChatMessage] = []
        for line_no, line in enumerate(raw.splitlines(), start=1):
            line = line.strip()
            if not line:
                continue
            try:
                messages.append(ChatMessage.model_validate_json(line))
            except ValueError as e:
                # One corrupt line must not lose the rest of the day.
                logger.error("Skipping bad message at %s:%d: %s", path, line_no, e)
        return messages

    def load_conversation(self, date_str: str) -> DailyConversation:
        """Load one day's conversation from disk, or return an empty one."""
        path = self._get_daily_file(date_str)
        if not path.exists():
            path = self._legacy_daily_file(date_str)
        messages = self._read_messages(path) if path.exists() else []

        summary_path = self._summary_file(date_str)
        summary = (
            summary_path.read_text(encoding="utf-8") if summary_path.exists() else ""
        )

        return DailyConversation(date=date_str, messages=messages, summary=summary)

    def load_today_conversation(self) -> DailyConversation:
        """Load today's conversation from disk, or create a new one."""
        return self.load_conversation(self._get_today_str())

    def load_recent_messages(self, limit: int = 20) -> list[ChatMessage]:
        """Load the most recent messages across days up to the limit."""
        messages: list[ChatMessage] = []
        # Walk backwards from the newest day until we have enough.
        for _date_str, path in reversed(self._dated_files()):
            for msg in reversed(self._read_messages(path)):
                messages.append(msg)
                if len(messages) >= limit:
                    break
            if len(messages) >= limit:
                break

        # Reverse back so the oldest is first, newest is last
        return list(reversed(messages))

    # ── Writing ──────────────────────────────────────────────────────────────

    def save_conversation(self, conv: DailyConversation) -> None:
        """Replace a whole day's conversation on disk."""
        path = self._get_daily_file(conv.date)
        body = "".join(msg.model_dump_json() + "\n" for msg in conv.messages)
        try:
            with self._locked():
                self._atomic_write(path, body)
                if conv.summary:
                    self._atomic_write(self._summary_file(conv.date), conv.summary)
                # The JSONL file is now authoritative for this day.
                self._legacy_daily_file(conv.date).unlink(missing_ok=True)
        except OSError as e:
            logger.error("Failed to save conversation to %s: %s", path, e)

    def append_message(self, role: str, content: str) -> None:
        """Append a single message to today's conversation.

        This is an O(1) append under the scope lock — it never rewrites the
        day, so a long conversation costs linear total I/O rather than
        quadratic.
        """
        today = self._get_today_str()
        msg = ChatMessage(role=role, content=content)
        path = self._get_daily_file(today)
        try:
            with self._locked():
                self._migrate_legacy_day(today)
                with open(path, "a", encoding="utf-8") as handle:
                    handle.write(msg.model_dump_json() + "\n")
        except OSError as e:
            logger.error("Failed to append message to %s: %s", path, e)

    def _migrate_legacy_day(self, date_str: str) -> None:
        """Fold a pre-JSONL day file into the JSONL file. Caller holds the lock."""
        legacy = self._legacy_daily_file(date_str)
        if not legacy.exists():
            return
        path = self._get_daily_file(date_str)
        existing = self._read_messages(path) if path.exists() else []
        merged = self._read_messages(legacy) + existing
        self._atomic_write(
            path, "".join(msg.model_dump_json() + "\n" for msg in merged)
        )
        legacy.unlink(missing_ok=True)
        logger.info("Migrated legacy conversation %s to JSONL", legacy.name)

    def _atomic_write(self, path: Path, body: str) -> None:
        """Write ``body`` to ``path`` via a temp file + rename. Caller holds the lock."""
        fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}-")
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            handle.write(body)
        os.replace(tmp, path)

    # ── Facts ────────────────────────────────────────────────────────────────

    def read_facts(self) -> str:
        """Read long-term facts."""
        if not self.facts_file.exists():
            return ""
        return self.facts_file.read_text(encoding="utf-8")

    def append_fact(self, fact: str) -> None:
        """Append a new long-term fact."""
        with self._locked_facts():
            content = self.read_facts()
            if content and not content.endswith("\n"):
                content += "\n"
            content += f"- {fact}\n"
            self._atomic_write(self.facts_file, content)
