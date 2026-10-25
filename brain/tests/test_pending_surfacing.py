"""Actions queued by the MCP server process during a turn must reach the reply."""

from pathlib import Path
from typing import Any

import pytest

from nerv.memory.manager import scope_for
from nerv.models import RouteResult
from nerv.orchestrator.orchestrator import Orchestrator


async def test_reply_mentions_actions_queued_by_mcp_during_turn(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    orch = Orchestrator(tmp_path)
    scope = scope_for("telegram", "alice")

    async def fake_call_model(*args: Any, **kwargs: Any) -> str:
        # Simulates the MCP server (another process) suspending a tool call
        # against the shared file-backed store while claude-cli runs.
        orch._pending_store.add(
            "shell",
            {"command": "rm -rf build"},
            reason="shell commands need confirmation",
            sender="alice",
            channel="telegram",
            scope=scope,
        )
        # A different sender's action must never leak into this reply.
        orch._pending_store.add(
            "shell",
            {"command": "echo other"},
            reason="other sender",
            sender="bob",
            channel="telegram",
            scope=scope_for("telegram", "bob"),
        )
        return "I tried to run it."

    async def fake_find_agent(route: Any, hint: str) -> Any:
        return orch.registry.get_default()

    monkeypatch.setattr(orch, "_find_agent", fake_find_agent)
    monkeypatch.setattr(orch, "_call_model", fake_call_model)
    monkeypatch.setattr(orch, "_select_model", lambda tier: "claude-cli:opus")

    reply = await orch.dispatch(
        "clean the build dir",
        RouteResult(intent="task"),
        channel="telegram",
        sender="alice",
    )

    assert "I tried to run it." in reply
    assert "rm -rf build" in reply
    assert "shell commands need confirmation" in reply
    assert "confirm" in reply
    assert "echo other" not in reply
