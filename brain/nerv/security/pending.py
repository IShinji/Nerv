"""File-backed store for suspended high-risk actions awaiting user approval.

The store is shared across processes: the Nerv MCP server (spawned by the
Claude Code CLI) enqueues suspended actions here, and the orchestrator (a
separate process) lists, executes, or cancels them when the user replies with
``confirm <id>`` / ``cancel <id>``. Access is serialized with a file lock.
"""

from __future__ import annotations

import contextlib
import json
import os
import tempfile
import time
from collections.abc import Iterator
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

from nerv.security.locks import exclusive_lock
from nerv.state_version import CURRENT_SCHEMA_VERSION, is_newer_schema


@dataclass
class PendingRecord:
    """A suspended tool invocation awaiting user confirmation."""

    id: int
    tool_name: str
    arguments: dict[str, Any]
    reason: str = ""
    sender: str = ""
    channel: str = ""
    scope: str = ""
    created_at: float = 0.0
    justification: str = ""


# A suspended action that nobody confirmed within this window is dropped, so a
# stale approval can never fire long after the user forgot about it.
DEFAULT_PENDING_TTL_SECONDS = 3600.0


MAX_JUSTIFICATION_CHARS = 300


def clean_justification(value: object) -> str:
    """Normalise a model-supplied justification for display.

    Collapses all whitespace (so it cannot fake extra chat lines) and caps the
    length. Non-strings become empty.
    """
    if not isinstance(value, str):
        return ""
    text = " ".join(value.split())
    if len(text) > MAX_JUSTIFICATION_CHARS:
        text = text[: MAX_JUSTIFICATION_CHARS - 3].rstrip() + "..."
    return text


class NewerSchemaError(RuntimeError):
    """The state file was written by a newer Nerv; it must not be modified."""


class PendingActionStore:
    """JSON-file queue of pending actions, safe for concurrent processes."""

    def __init__(
        self, project_root: Path, ttl_seconds: float = DEFAULT_PENDING_TTL_SECONDS
    ) -> None:
        self._ttl = ttl_seconds
        self._dir = project_root / "personal"
        self._path = self._dir / "pending_actions.json"

    @contextlib.contextmanager
    def _locked(self) -> Iterator[None]:
        """Hold an exclusive cross-process lock for a read-modify-write cycle."""
        self._dir.mkdir(parents=True, exist_ok=True)
        with exclusive_lock(self._dir, "pending_actions.lock"):
            yield

    def _read(self) -> dict[str, Any]:
        if not self._path.exists():
            return {"next_id": 1, "actions": []}
        try:
            data = json.loads(self._path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            return {"next_id": 1, "actions": []}
        if is_newer_schema(data, self._path):
            raise NewerSchemaError(
                "pending_actions.json was written by a newer version of Nerv"
            )
        return data

    def _write(self, data: dict[str, Any]) -> None:
        self._dir.mkdir(parents=True, exist_ok=True)
        fd, tmp = tempfile.mkstemp(dir=self._dir, prefix=".pending-", suffix=".json")
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(
                {**data, "schema_version": CURRENT_SCHEMA_VERSION},
                handle,
                ensure_ascii=False,
                indent=2,
            )
        os.replace(tmp, self._path)

    def add(
        self,
        tool_name: str,
        arguments: dict[str, Any],
        reason: str = "",
        sender: str = "",
        channel: str = "",
        scope: str = "",
        justification: str = "",
    ) -> PendingRecord:
        """Enqueue a suspended action and return its assigned record."""
        with self._locked():
            data = self._read()
            data["actions"] = self._live(data.get("actions", []))
            record = PendingRecord(
                id=int(data.get("next_id", 1)),
                tool_name=tool_name,
                arguments=dict(arguments),
                reason=reason,
                sender=sender,
                channel=channel,
                scope=scope,
                created_at=time.time(),
                justification=clean_justification(justification),
            )
            data["next_id"] = record.id + 1
            data.setdefault("actions", []).append(asdict(record))
            self._write(data)
            return record

    def _live(self, actions: list[dict[str, Any]]) -> list[dict[str, Any]]:
        """Drop expired actions. Records without a timestamp count as expired."""
        if self._ttl <= 0:
            return actions
        now = time.time()
        return [a for a in actions if now - float(a.get("created_at", 0.0)) < self._ttl]

    def list(self) -> list[PendingRecord]:
        """Return all non-expired pending actions."""
        try:
            data = self._read()
        except NewerSchemaError:
            return []
        known = set(PendingRecord.__dataclass_fields__)
        return [
            PendingRecord(**{k: v for k, v in item.items() if k in known})
            for item in self._live(data.get("actions", []))
        ]

    def get(self, action_id: int) -> PendingRecord | None:
        """Return one pending action by id, or ``None``."""
        return next((rec for rec in self.list() if rec.id == action_id), None)

    def remove(self, action_id: int) -> PendingRecord | None:
        """Remove and return a pending action by id, or ``None`` if absent."""
        with self._locked():
            try:
                data = self._read()
            except NewerSchemaError:
                return None
            actions = data.get("actions", [])
            removed = next(
                (a for a in self._live(actions) if a.get("id") == action_id), None
            )
            if removed is None:
                return None
            data["actions"] = [a for a in actions if a.get("id") != action_id]
            self._write(data)
            known = set(PendingRecord.__dataclass_fields__)
            return PendingRecord(**{k: v for k, v in removed.items() if k in known})
