"""A brain killed mid-request leaves its temp MCP config behind; the next one clears it."""

from __future__ import annotations

import os
import time
from pathlib import Path

from nerv.llm.claude_cli import sweep_stale_mcp_configs


def _file(path: Path, age_seconds: float, now: float) -> Path:
    path.write_text("{}", encoding="utf-8")
    stamp = now - age_seconds
    os.utime(path, (stamp, stamp))
    return path


def test_sweep_removes_only_stale_nerv_configs(tmp_path: Path) -> None:
    now = time.time()
    stale = _file(tmp_path / "nerv-mcp-abc123.json", 2 * 3600, now)
    live = _file(tmp_path / "nerv-mcp-def456.json", 60, now)  # a request in flight
    other = _file(tmp_path / "unrelated.json", 2 * 3600, now)
    unrelated_nerv = _file(tmp_path / "nerv-mcp-notes.txt", 2 * 3600, now)

    removed = sweep_stale_mcp_configs(tmp_path, now=now)

    assert removed == 1
    assert not stale.exists()
    assert live.exists() and other.exists() and unrelated_nerv.exists()


def test_sweep_never_follows_a_symlink(tmp_path: Path) -> None:
    now = time.time()
    precious = _file(tmp_path / "precious.txt", 2 * 3600, now)
    link = tmp_path / "nerv-mcp-link.json"
    link.symlink_to(precious)
    os.utime(link, (now - 2 * 3600, now - 2 * 3600), follow_symlinks=False)

    assert sweep_stale_mcp_configs(tmp_path, now=now) == 0

    assert link.is_symlink()
    assert precious.exists()


def test_sweep_copes_with_a_missing_directory(tmp_path: Path) -> None:
    assert sweep_stale_mcp_configs(tmp_path / "does-not-exist") == 0
