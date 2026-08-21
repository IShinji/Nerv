"""Tests for the claude-cli backend: retry, metadata, and message flattening."""

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
