"""Tests for the Nerv MCP tool server (sandbox-gated tool execution)."""

from pathlib import Path

import pytest

from nerv.mcp_server.server import build_server, execute_or_suspend
from nerv.security import PendingActionStore


def test_build_server_exposes_native_tools(tmp_path: Path) -> None:
    server = build_server(tmp_path)
    assert server.name == "nerv"


@pytest.mark.asyncio
async def test_server_exposes_productivity_tools(tmp_path: Path) -> None:
    """Regression: the MCP server process must register productivity tools too."""
    import mcp.types as types

    server = build_server(tmp_path)
    handler = server.request_handlers[types.ListToolsRequest]
    res = await handler(types.ListToolsRequest(method="tools/list"))
    names = {t.name for t in res.root.tools}
    assert {"calendar_add", "note_write", "send_email", "evolve_agent"} <= names


@pytest.mark.asyncio
async def test_safe_tool_executes(tmp_path: Path) -> None:
    store = PendingActionStore(tmp_path)
    out = await execute_or_suspend("current_time", {}, tmp_path, store)
    assert "Current time" in out
    assert store.list() == []


@pytest.mark.asyncio
async def test_out_of_sandbox_write_is_suspended(tmp_path: Path) -> None:
    store = PendingActionStore(tmp_path)
    out = await execute_or_suspend(
        "write_file_full",
        {"absolute_path": "/tmp/escape.txt", "content": "x"},
        tmp_path,
        store,
    )
    assert "ACTION SUSPENDED" in out
    assert len(store.list()) == 1
    assert store.list()[0].tool_name == "write_file_full"


@pytest.mark.asyncio
async def test_shell_is_suspended(tmp_path: Path) -> None:
    store = PendingActionStore(tmp_path)
    out = await execute_or_suspend("shell", {"command": "ls"}, tmp_path, store)
    assert "ACTION SUSPENDED" in out
    assert len(store.list()) == 1


@pytest.mark.asyncio
async def test_in_sandbox_write_executes(tmp_path: Path) -> None:
    store = PendingActionStore(tmp_path)
    target = tmp_path / "note.txt"
    out = await execute_or_suspend(
        "write_file_full",
        {"absolute_path": str(target), "content": "hello"},
        tmp_path,
        store,
    )
    assert "ACTION SUSPENDED" not in out
    assert target.read_text() == "hello"
    assert store.list() == []


@pytest.mark.asyncio
async def test_unknown_tool_reported(tmp_path: Path) -> None:
    store = PendingActionStore(tmp_path)
    out = await execute_or_suspend("nonexistent_tool", {}, tmp_path, store)
    assert "Unknown tool" in out
