"""Oversized tool output is spilled to a private file, not silently dropped."""

from __future__ import annotations

import os
import pathlib
import re
import stat
import time

import pytest

from nerv.security.sandbox import is_secret_path
from nerv.tools import _helpers
from nerv.tools._helpers import MAX_TOOL_OUTPUT_CHARS, _truncate


@pytest.fixture
def spill_dir(tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch) -> pathlib.Path:
    target = tmp_path / "spill"
    monkeypatch.setenv("NERV_SPILL_DIR", str(target))
    return target


def _big() -> str:
    return "HEAD" + "x" * (MAX_TOOL_OUTPUT_CHARS * 3) + "TAIL"


def test_under_limit_is_unchanged(spill_dir: pathlib.Path) -> None:
    assert _truncate("short") == "short"
    assert not spill_dir.exists()


def test_over_limit_returns_preview_and_path(spill_dir: pathlib.Path) -> None:
    text = _big()
    out = _truncate(text, tool_name="shell")
    assert out.startswith("HEAD")
    assert out.endswith("TAIL")
    assert len(out) < len(text)
    files = list(spill_dir.iterdir())
    assert len(files) == 1
    assert str(files[0]) in out
    assert str(len(text) - 4000) in out or "characters omitted" in out
    assert "read_file" in out
    assert files[0].read_text(encoding="utf-8") == text
    assert stat.S_IMODE(files[0].stat().st_mode) == 0o600
    assert stat.S_IMODE(spill_dir.stat().st_mode) == 0o700
    assert re.fullmatch(r"spill-shell-\d{8}T\d{6}-[0-9a-f]{8}\.txt", files[0].name)


def test_spill_location_is_readable_by_file_tools(
    spill_dir: pathlib.Path, tmp_path: pathlib.Path
) -> None:
    _truncate(_big())
    spilled = next(spill_dir.iterdir())
    assert not is_secret_path(str(spilled), tmp_path)


def test_default_location_is_not_secret(
    monkeypatch: pytest.MonkeyPatch, tmp_path: pathlib.Path
) -> None:
    monkeypatch.delenv("NERV_SPILL_DIR", raising=False)
    assert not is_secret_path(str(_helpers._spill_dir() / "x.txt"), tmp_path)


def test_symlink_in_the_way_does_not_crash(
    spill_dir: pathlib.Path, monkeypatch: pytest.MonkeyPatch, tmp_path: pathlib.Path
) -> None:
    spill_dir.mkdir(mode=0o700)
    victim = tmp_path / "victim.txt"
    victim.write_text("keep")
    monkeypatch.setattr(_helpers.secrets, "token_hex", lambda n: "deadbeef")
    monkeypatch.setattr(_helpers.time, "time", lambda: 1_700_000_000.0)
    stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime(1_700_000_000.0))
    (spill_dir / f"spill-tool-{stamp}-deadbeef.txt").symlink_to(victim)
    out = _truncate(_big())
    assert out.startswith("HEAD") and out.endswith("TAIL")
    assert "failed" in out
    assert victim.read_text() == "keep"


def test_symlinked_spill_dir_is_refused(
    spill_dir: pathlib.Path, tmp_path: pathlib.Path
) -> None:
    real = tmp_path / "elsewhere"
    real.mkdir()
    spill_dir.symlink_to(real)
    out = _truncate(_big())
    assert "failed" in out
    assert list(real.iterdir()) == []


def test_old_spill_files_are_cleaned(spill_dir: pathlib.Path) -> None:
    spill_dir.mkdir(mode=0o700)
    old = spill_dir / "spill-shell-20200101T000000-aaaaaaaa.txt"
    fresh = spill_dir / "spill-shell-20200101T000000-bbbbbbbb.txt"
    other = spill_dir / "notes.txt"
    for f in (old, fresh, other):
        f.write_text("x")
    long_ago = time.time() - (_helpers.SPILL_RETENTION_DAYS + 1) * 86400
    os.utime(old, (long_ago, long_ago))
    os.utime(other, (long_ago, long_ago))
    _truncate(_big())
    assert not old.exists()
    assert fresh.exists()
    assert other.exists()
