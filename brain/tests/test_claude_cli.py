"""Tests for the claude-cli backend: retry, metadata, and message flattening."""

import asyncio

import pytest

from nerv.llm import claude_cli
from nerv.llm.claude_cli import _flatten_messages, complete_claude_cli


def test_flatten_messages_splits_system_and_conversation() -> None:
    system, convo = _flatten_messages(
        [
            {"role": "system", "content": "You are Nerv."},
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello"},
            {"role": "tool", "content": "result data"},
        ]
    )
    assert system == "You are Nerv."
    assert "hi" in convo
    assert "Assistant: hello" in convo
    assert "[Tool result]: result data" in convo


@pytest.mark.asyncio
async def test_retries_then_succeeds(monkeypatch) -> None:
    monkeypatch.setenv("NERV_CLAUDE_MAX_ATTEMPTS", "3")
    monkeypatch.setattr(claude_cli, "_backoff_seconds", lambda attempt: 0.0)

    calls = {"n": 0}

    async def flaky(args, conversation, timeout):
        calls["n"] += 1
        if calls["n"] < 2:
            raise RuntimeError("transient")
        return {"result": "ok", "duration_ms": 5, "total_cost_usd": 0.01}

    monkeypatch.setattr(claude_cli, "_invoke_once", flaky)

    result = await complete_claude_cli(
        [{"role": "user", "content": "hi"}], model_name="opus"
    )
    assert result.content == "ok"
    assert result.runs_own_tool_loop is True
    assert result.cost_usd == 0.01
    assert calls["n"] == 2


@pytest.mark.asyncio
async def test_raises_after_exhausting_attempts(monkeypatch) -> None:
    monkeypatch.setenv("NERV_CLAUDE_MAX_ATTEMPTS", "2")
    monkeypatch.setattr(claude_cli, "_backoff_seconds", lambda attempt: 0.0)

    async def always_fail(args, conversation, timeout):
        raise RuntimeError("boom")

    monkeypatch.setattr(claude_cli, "_invoke_once", always_fail)

    with pytest.raises(RuntimeError, match="failed after 2 attempt"):
        await complete_claude_cli(
            [{"role": "user", "content": "hi"}], model_name="opus"
        )


@pytest.mark.asyncio
async def test_error_result_retried(monkeypatch) -> None:
    monkeypatch.setenv("NERV_CLAUDE_MAX_ATTEMPTS", "2")
    monkeypatch.setattr(claude_cli, "_backoff_seconds", lambda attempt: 0.0)

    seq = [
        {"is_error": True, "subtype": "overloaded", "result": ""},
        {"result": "recovered", "duration_ms": 3},
    ]

    async def two_phase(args, conversation, timeout):
        return seq.pop(0)

    monkeypatch.setattr(claude_cli, "_invoke_once", two_phase)

    result = await complete_claude_cli(
        [{"role": "user", "content": "hi"}], model_name="opus"
    )
    assert result.content == "recovered"


@pytest.mark.asyncio
async def test_args_isolate_from_user_environment(monkeypatch) -> None:
    seen: dict = {}

    async def capture(args, conversation, timeout):
        seen["args"] = args
        return {"result": "ok"}

    monkeypatch.setattr(claude_cli, "_invoke_once", capture)
    await complete_claude_cli([{"role": "user", "content": "hi"}], model_name="opus")
    args = seen["args"]
    assert "--strict-mcp-config" in args
    assert "--no-session-persistence" in args
    assert args[args.index("--tools") + 1] == ""
    assert args[args.index("--setting-sources") + 1] == ""
    assert "--disallowedTools" not in args


@pytest.mark.asyncio
async def test_timeout_kills_child_process(monkeypatch) -> None:
    killed = {"v": False}

    class FakeProc:
        returncode = None

        async def communicate(self, data):
            await asyncio.sleep(10)

        def kill(self):
            killed["v"] = True

        async def wait(self):
            return 0

    async def fake_exec(*a, **k):
        return FakeProc()

    monkeypatch.setattr(asyncio, "create_subprocess_exec", fake_exec)
    with pytest.raises(RuntimeError, match="timed out"):
        await claude_cli._invoke_once(["claude"], "hi", 0.05)
    assert killed["v"]
