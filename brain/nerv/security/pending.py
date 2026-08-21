"""File-backed store for suspended high-risk actions awaiting user approval.

The store is shared across processes: the Nerv MCP server (spawned by the
Claude Code CLI) enqueues suspended actions here, and the orchestrator (a
separate process) lists, executes, or cancels them when the user replies with
``confirm <id>`` / ``cancel <id>``. Access is serialized with a file lock.
"""

from __future__ import annotations

import contextlib
import fcntl
import json
import os
import tempfile
from collections.abc import Iterator
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any


@dataclass
class PendingRecord:
    """A suspended tool invocation awaiting user confirmation."""

    id: int
    tool_name: str
    arguments: dict[str, Any]
    reason: str = ""
    sender: str = ""
    channel: str = ""


class PendingActionStore:
    """JSON-file queue of pending actions, safe for concurrent processes."""

    def __init__(self, project_root: Path) -> None:
        self._dir = project_root / "personal"
        self._path = self._dir / "pending_actions.json"
        self._lock_path = self._dir / "pending_actions.lock"

    @contextlib.contextmanager
    def _locked(self) -> Iterator[None]:
        """Hold an exclusive cross-process lock for a read-modify-write cycle."""
        self._dir.mkdir(parents=True, exist_ok=True)
        with open(self._lock_path, "w") as lock_file:
            fcntl.flock(lock_file, fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(lock_file, fcntl.LOCK_UN)

    def _read(self) -> dict[str, Any]:
        if not self._path.exists():
            return {"next_id": 1, "actions": []}
        try:
            return json.loads(self._path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            return {"next_id": 1, "actions": []}

    def _write(self, data: dict[str, Any]) -> None:
        self._dir.mkdir(parents=True, exist_ok=True)
        fd, tmp = tempfile.mkstemp(dir=self._dir, prefix=".pending-", suffix=".json")
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(data, handle, ensure_ascii=False, indent=2)
        os.replace(tmp, self._path)

    def add(
        self,
        tool_name: str,
        arguments: dict[str, Any],
        reason: str = "",
        sender: str = "",
        channel: str = "",
    ) -> PendingRecord:
        """Enqueue a suspended action and return its assigned record."""
        with self._locked():
            data = self._read()
            record = PendingRecord(
                id=int(data.get("next_id", 1)),
                tool_name=tool_name,
                arguments=dict(arguments),
                reason=reason,
                sender=sender,
                channel=channel,
            )
            data["next_id"] = record.id + 1
            data.setdefault("actions", []).append(asdict(record))
            self._write(data)
            return record

    def list(self) -> list[PendingRecord]:
        """Return all pending actions."""
        data = self._read()
        return [PendingRecord(**item) for item in data.get("actions", [])]

    def get(self, action_id: int) -> PendingRecord | None:
        """Return one pending action by id, or ``None``."""
        return next((rec for rec in self.list() if rec.id == action_id), None)

    def remove(self, action_id: int) -> PendingRecord | None:
        """Remove and return a pending action by id, or ``None`` if absent."""
        with self._locked():
            data = self._read()
            actions = data.get("actions", [])
            removed = next((a for a in actions if a.get("id") == action_id), None)
            if removed is None:
                return None
            data["actions"] = [a for a in actions if a.get("id") != action_id]
            self._write(data)
            return PendingRecord(**removed)
