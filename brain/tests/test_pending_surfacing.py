"""Actions queued by the MCP server process during a turn must reach the reply."""

from pathlib import Path

from nerv.memory.manager import scope_for
from nerv.orchestrator.orchestrator import Orchestrator


async def test_finish_turn_mentions_actions_queued_by_mcp_during_turn(
    tmp_path: Path,
) -> None:
    orch = Orchestrator(tmp_path)
    scope = scope_for("telegram", "alice")

    # Simulates the MCP server (another process) suspending a tool call
    # against the shared file-backed store while the model runs.
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

    result = await orch.finish_turn(
        "clean the build dir",
        "I tried to run it.",
        channel="telegram",
        sender="alice",
        known_pending=[],
    )

    reply = result["reply"]
    assert result["pending_created"] is True
    assert "I tried to run it." in reply
    assert "rm -rf build" in reply
    assert "shell commands need confirmation" in reply
    assert "confirm" in reply
    assert "echo other" not in reply


async def test_finish_turn_stays_quiet_when_actions_were_already_known(
    tmp_path: Path,
) -> None:
    """A pending action the turn already knew about is not re-announced."""
    orch = Orchestrator(tmp_path)
    scope = scope_for("telegram", "alice")
    orch._pending_store.add(
        "shell",
        {"command": "rm -rf build"},
        reason="shell commands need confirmation",
        sender="alice",
        channel="telegram",
        scope=scope,
    )
    known = [{"id": a.id} for a in orch._visible_pending_actions("alice", "telegram")]

    result = await orch.finish_turn(
        "clean the build dir",
        "I tried to run it.",
        channel="telegram",
        sender="alice",
        known_pending=known,
    )

    assert result["reply"] == "I tried to run it."
    assert result["pending_created"] is False
