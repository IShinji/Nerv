"""Self-evolving agents: versioned agent definitions with rollback.

Agents are YAML files. Before any change, the current definition is snapshotted
into ``personal/agents/history/<name>/`` with a note, so an agent's growth is an
auditable, reversible history rather than a destructive overwrite.
"""

from __future__ import annotations

import json
import time
from dataclasses import dataclass
from pathlib import Path


@dataclass
class VersionInfo:
    """One archived version of an agent definition."""

    version: str
    ts: float
    note: str


class AgentVersionStore:
    """Snapshot, list, and roll back agent definition versions."""

    def __init__(self, project_root: Path) -> None:
        self.agents_dir = project_root / "personal" / "agents"
        self.history_dir = self.agents_dir / "history"

    def find_file(self, agent_name: str) -> Path | None:
        """Locate an agent's YAML by filename stem or its ``name:`` field."""
        if not self.agents_dir.exists():
            return None
        target = agent_name.strip().lower()
        # Match by filename stem first (cheap, exact).
        by_stem = self.agents_dir / f"{target}.yaml"
        if by_stem.exists():
            return by_stem
        # Otherwise match the `name:` field, case-insensitively.
        for path in self.agents_dir.glob("*.yaml"):
            for line in path.read_text(encoding="utf-8").splitlines():
                stripped = line.strip()
                if stripped.lower().startswith("name:"):
                    value = stripped.split(":", 1)[1].strip().strip('"').strip("'")
                    if value.lower() == target:
                        return path
                    break
        return None

    def _agent_history_dir(self, file: Path) -> Path:
        return self.history_dir / file.stem

    def _index_path(self, file: Path) -> Path:
        return self._agent_history_dir(file) / "index.json"

    def _read_index(self, file: Path) -> list[dict]:
        path = self._index_path(file)
        if not path.exists():
            return []
        try:
            return json.loads(path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            return []

    def snapshot(self, agent_name: str, note: str = "") -> str | None:
        """Archive the current agent definition; return the new version id."""
        file = self.find_file(agent_name)
        if file is None:
            return None

        hist = self._agent_history_dir(file)
        hist.mkdir(parents=True, exist_ok=True)
        version = time.strftime("%Y%m%dT%H%M%S")
        # Disambiguate if two snapshots land in the same second.
        snap_path = hist / f"{version}.yaml"
        suffix = 1
        while snap_path.exists():
            version = f"{version}-{suffix}"
            snap_path = hist / f"{version}.yaml"
            suffix += 1

        snap_path.write_text(file.read_text(encoding="utf-8"), encoding="utf-8")
        index = self._read_index(file)
        index.append({"version": version, "ts": time.time(), "note": note})
        self._index_path(file).write_text(
            json.dumps(index, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        return version

    def list_versions(self, agent_name: str) -> list[VersionInfo]:
        """Return archived versions for an agent, newest last."""
        file = self.find_file(agent_name)
        if file is None:
            return []
        return [VersionInfo(**entry) for entry in self._read_index(file)]

    def rollback(self, agent_name: str, version: str) -> bool:
        """Restore an archived version as the live definition.

        Snapshots the current state first, so a rollback is itself reversible.
        """
        file = self.find_file(agent_name)
        if file is None:
            return False
        snap_path = self._agent_history_dir(file) / f"{version}.yaml"
        if not snap_path.exists():
            return False

        self.snapshot(agent_name, note=f"pre-rollback to {version}")
        file.write_text(snap_path.read_text(encoding="utf-8"), encoding="utf-8")
        return True
