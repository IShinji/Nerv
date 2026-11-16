"""One trace line per request, with no content in it."""

from __future__ import annotations

import json
import stat
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from nerv import request_trace
from nerv.models import RouteResult
from nerv.orchestrator.orchestrator import Orchestrator

SENTINEL = "SENTINEL-SECRET-123"
FIELDS = {
    "ts",
    "channel",
    "scope",
    "tier",
    "model",
    "provider",
    "est_input_tokens",
    "tools_called",
    "duration_ms",
    "cost_usd",
    "pending_created",
    "delegated",
    "error",
}


@pytest.fixture(autouse=True)
def _general_agent(tmp_path: Path) -> None:
    """Provide a real agent so no model call is spent creating one."""
    agents = tmp_path / "agents"
    agents.mkdir()
    (agents / "general.yaml").write_text(
        "name: General\ndescription: General assistant\ntags: [general]\n"
    )


def _trace_path(root: Path) -> Path:
    return root / "personal" / "logs" / "trace.jsonl"


def _lines(root: Path) -> list[dict[str, Any]]:
    text = _trace_path(root).read_text(encoding="utf-8")
    return [json.loads(line) for line in text.splitlines()]


def _fake_complete(monkeypatch: pytest.MonkeyPatch, replies: list[Any]) -> None:
    async def fake(messages: Any, **kwargs: Any) -> Any:
        return replies.pop(0)

    monkeypatch.setattr("nerv.llm.complete", fake)


async def test_one_request_writes_exactly_one_line(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    orch = Orchestrator(tmp_path)
    _fake_complete(
        monkeypatch,
        [
            SimpleNamespace(
                content="hello",
                tool_calls=[],
                runs_own_tool_loop=True,
                provider="claude-cli",
                cost_usd=0.05,
            )
        ],
    )
    reply = await orch.dispatch(
        "hi there",
        RouteResult(intent="general", model_tier=2),
        channel="telegram",
        sender="user-1",
    )
    assert reply == "hello"
    lines = _lines(tmp_path)
    assert len(lines) == 1
    record = lines[0]
    assert set(record) == FIELDS
    assert record["channel"] == "telegram"
    assert record["scope"] == "telegram-user-1"
    assert record["tier"] == 2
    assert record["provider"] == "claude-cli"
    assert record["cost_usd"] == 0.05
    assert record["pending_created"] is False
    assert record["delegated"] is False
    assert record["error"] is None
    assert record["est_input_tokens"] > 0
    assert record["duration_ms"] >= 0
    assert record["ts"].endswith("+00:00")


async def test_native_tools_are_recorded_by_name_only(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    orch = Orchestrator(tmp_path)
    monkeypatch.setenv("NERV_SENTINEL_ENV", SENTINEL)
    call = {"function": {"name": "current_time", "arguments": {"timezone": SENTINEL}}}
    _fake_complete(
        monkeypatch,
        [
            SimpleNamespace(content="", tool_calls=[call], runs_own_tool_loop=False),
            SimpleNamespace(
                content=f"reply {SENTINEL}", tool_calls=[], runs_own_tool_loop=False
            ),
        ],
    )
    await orch.dispatch(
        f"please use {SENTINEL}", RouteResult(intent="general"), channel="cli"
    )
    raw = _trace_path(tmp_path).read_text(encoding="utf-8")
    assert SENTINEL not in raw
    record = json.loads(raw)
    assert record["tools_called"] == ["current_time"]


async def test_failed_request_records_error_category_only(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    orch = Orchestrator(tmp_path)

    async def boom(messages: Any, **kwargs: Any) -> Any:
        raise RuntimeError(f"leaked {SENTINEL}")

    monkeypatch.setattr("nerv.llm.complete", boom)
    await orch.dispatch("hi", RouteResult(intent="general"), channel="cli")
    raw = _trace_path(tmp_path).read_text(encoding="utf-8")
    assert SENTINEL not in raw
    assert json.loads(raw)["error"] == "RuntimeError"


async def test_early_reply_path_still_one_line(tmp_path: Path) -> None:
    orch = Orchestrator(tmp_path)
    await orch.dispatch("hi", RouteResult(intent="chat", reply="direct"))
    assert len(_lines(tmp_path)) == 1


async def test_trace_file_permissions(tmp_path: Path) -> None:
    orch = Orchestrator(tmp_path)
    await orch.dispatch("hi", RouteResult(intent="chat", reply="direct"))
    path = _trace_path(tmp_path)
    assert stat.S_IMODE(path.stat().st_mode) == 0o600
    assert stat.S_IMODE(path.parent.stat().st_mode) == 0o700


def test_rotation_keeps_three_copies(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(request_trace, "TRACE_MAX_BYTES", 200)
    for _ in range(12):
        request_trace.write_trace(tmp_path, request_trace.RequestTrace(channel="c"))
    names = sorted(p.name for p in _trace_path(tmp_path).parent.iterdir())
    assert names == [
        "trace.jsonl",
        "trace.jsonl.1",
        "trace.jsonl.2",
        "trace.jsonl.3",
        "trace.lock",
    ]


async def test_write_failure_does_not_affect_the_reply(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    # personal/logs exists as a regular file, so the directory cannot be created.
    (tmp_path / "personal").mkdir()
    (tmp_path / "personal" / "logs").write_text("not a dir")
    orch = Orchestrator(tmp_path)
    reply = await orch.dispatch("hi", RouteResult(intent="chat", reply="direct"))
    assert reply == "direct"
    assert "Could not write request trace" in caplog.text


async def test_pending_created_flag(tmp_path: Path) -> None:
    orch = Orchestrator(tmp_path)
    with request_trace.request_trace("cli", "default") as trace:
        orch._queue_pending_action("shell", {"command": "ls"}, sender="", channel="")
    assert trace.pending_created is True
