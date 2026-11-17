"""Failure classification for the claude-cli backend (fake `claude` executable)."""

from __future__ import annotations

import json
import os
import pathlib
import stat
import sys

import pytest

from nerv.llm import claude_cli
from nerv.llm.claude_cli import classify_failure, complete_claude_cli

MESSAGES = [{"role": "user", "content": "hi"}]


def _fake_claude(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch, body: dict, code: int
) -> pathlib.Path:
    """Install a fake `claude` that counts invocations and prints ``body``."""
    counter = tmp_path / "calls.txt"
    script = tmp_path / "fake-claude"
    script.write_text(
        f"#!{sys.executable}\n"
        "import sys\n"
        f"open({str(counter)!r}, 'a').write('x')\n"
        "sys.stdin.read()\n"
        f"print({json.dumps(body)!r})\n"
        f"sys.exit({code})\n"
    )
    script.chmod(script.stat().st_mode | stat.S_IXUSR)
    monkeypatch.setattr(claude_cli, "CLAUDE_BIN", str(script))
    monkeypatch.setattr(claude_cli, "_backoff_seconds", lambda attempt: 0.0)
    return counter


def _calls(counter: pathlib.Path) -> int:
    return len(counter.read_text()) if counter.exists() else 0


def _err(status: int | None, text: str, error_type: str = "") -> dict:
    return {
        "is_error": True,
        "subtype": "success",
        "api_error_status": status,
        "api_error_type": error_type,
        "result": text,
    }


@pytest.mark.asyncio
async def test_auth_failure_is_not_retried(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("NERV_CLAUDE_MAX_ATTEMPTS", "3")
    counter = _fake_claude(
        tmp_path, monkeypatch, _err(None, "Not logged in - Please run /login"), 1
    )
    with pytest.raises(RuntimeError, match="not logged in"):
        await complete_claude_cli(MESSAGES, model_name="opus")
    assert _calls(counter) == 1


@pytest.mark.asyncio
async def test_invalid_model_is_not_retried(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("NERV_CLAUDE_MAX_ATTEMPTS", "3")
    counter = _fake_claude(
        tmp_path,
        monkeypatch,
        _err(404, "issue with the selected model", "not_found_error"),
        1,
    )
    with pytest.raises(RuntimeError, match="rejected the request"):
        await complete_claude_cli(MESSAGES, model_name="nope")
    assert _calls(counter) == 1


@pytest.mark.asyncio
async def test_quota_exhausted_is_not_retried(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("NERV_CLAUDE_MAX_ATTEMPTS", "3")
    counter = _fake_claude(
        tmp_path, monkeypatch, _err(429, "Claude AI usage limit reached|1700000000"), 1
    )
    with pytest.raises(RuntimeError, match="quota"):
        await complete_claude_cli(MESSAGES, model_name="opus")
    assert _calls(counter) == 1


@pytest.mark.asyncio
async def test_5xx_retried_up_to_limit(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("NERV_CLAUDE_MAX_ATTEMPTS", "3")
    counter = _fake_claude(tmp_path, monkeypatch, _err(529, "Overloaded"), 1)
    with pytest.raises(RuntimeError, match="failed after 3 attempt"):
        await complete_claude_cli(MESSAGES, model_name="opus")
    # exit code 1 with an error JSON on stdout still carries the classification
    assert _calls(counter) == 3


@pytest.mark.asyncio
async def test_rate_limit_429_is_retried(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("NERV_CLAUDE_MAX_ATTEMPTS", "3")
    counter = _fake_claude(tmp_path, monkeypatch, _err(429, "Rate limited"), 1)
    with pytest.raises(RuntimeError):
        await complete_claude_cli(MESSAGES, model_name="opus")
    assert _calls(counter) == 3


@pytest.mark.asyncio
async def test_success_is_not_retried(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    counter = _fake_claude(tmp_path, monkeypatch, {"result": "ok"}, 0)
    result = await complete_claude_cli(MESSAGES, model_name="opus")
    assert result.content == "ok"
    assert _calls(counter) == 1


@pytest.mark.asyncio
async def test_unknown_error_retries_once_only(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("NERV_CLAUDE_MAX_ATTEMPTS", "4")
    counter = _fake_claude(tmp_path, monkeypatch, _err(None, "something odd"), 1)
    with pytest.raises(RuntimeError):
        await complete_claude_cli(MESSAGES, model_name="opus")
    assert _calls(counter) == 2


@pytest.mark.asyncio
async def test_timeout_is_retried_to_limit(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("NERV_CLAUDE_MAX_ATTEMPTS", "3")
    monkeypatch.setattr(claude_cli, "_backoff_seconds", lambda attempt: 0.0)
    calls = {"n": 0}

    async def timing_out(args, conversation, timeout):
        calls["n"] += 1
        raise RuntimeError("claude CLI timed out after 1s")

    monkeypatch.setattr(claude_cli, "_invoke_once", timing_out)
    with pytest.raises(RuntimeError, match="failed after 3 attempt"):
        await complete_claude_cli(MESSAGES, model_name="opus")
    assert calls["n"] == 3


@pytest.mark.parametrize(
    ("status", "text", "expected"),
    [
        (401, "", "fatal"),
        (None, "Invalid API key", "fatal"),
        (400, "bad", "fatal"),
        (402, "", "fatal"),
        (500, "", "retry"),
        (503, "", "retry"),
        (None, "ECONNRESET while connecting", "retry"),
        (None, "???", "unknown"),
    ],
)
def test_classify_failure(status: int | None, text: str, expected: str) -> None:
    kind, _message = classify_failure(status=status, error_type="", text=text)
    assert kind == expected


def test_backoff_has_bounded_jitter() -> None:
    values = {claude_cli._backoff_seconds(2) for _ in range(50)}
    assert len(values) > 1
    assert all(1.8 <= v <= 2.2 for v in values)
    assert os.environ is not None
