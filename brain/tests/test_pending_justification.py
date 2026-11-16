"""Models can say why an action needs confirmation; the user sees it."""

from __future__ import annotations

from pathlib import Path

import mcp.types as types

from nerv.mcp_server.server import build_server, execute_or_suspend
from nerv.orchestrator.orchestrator import Orchestrator
from nerv.security import PendingActionStore
from nerv.security.pending import MAX_JUSTIFICATION_CHARS, clean_justification


async def _list_tools(tmp_path: Path) -> dict[str, types.Tool]:
    server = build_server(tmp_path)
    handler = server.request_handlers[types.ListToolsRequest]
    res = await handler(types.ListToolsRequest(method="tools/list"))
    return {t.name: t for t in res.root.tools}


async def test_confirmable_tools_offer_justification(tmp_path: Path) -> None:
    tools = await _list_tools(tmp_path)
    for name in ("shell", "send_email", "write_file_full", "desktop_control"):
        props = tools[name].inputSchema["properties"]
        assert props["justification"]["type"] == "string", name
        assert "justification" not in tools[name].inputSchema.get("required", [])
    # A plain read-only tool does not grow the parameter.
    assert "justification" not in tools["current_time"].inputSchema.get(
        "properties", {}
    )


async def test_call_tool_stores_justification_and_strips_it(tmp_path: Path) -> None:
    server = build_server(tmp_path)
    handler = server.request_handlers[types.CallToolRequest]
    req = types.CallToolRequest(
        method="tools/call",
        params=types.CallToolRequestParams(
            name="shell",
            arguments={"command": "ls", "justification": "list the folder"},
        ),
    )
    await handler(req)
    [record] = PendingActionStore(tmp_path).list()
    assert record.justification == "list the folder"
    assert record.arguments == {"command": "ls"}


async def test_justification_is_stripped_before_running_a_tool(
    tmp_path: Path,
) -> None:
    store = PendingActionStore(tmp_path)
    target = tmp_path / "note.txt"
    out = await execute_or_suspend(
        "write_file_full",
        {
            "absolute_path": str(target),
            "content": "hi",
            "justification": "save a note",
        },
        tmp_path,
        store,
    )
    assert "ACTION SUSPENDED" not in out
    assert target.read_text() == "hi"


def test_clean_justification_normalizes() -> None:
    assert clean_justification("  a\nb\r\n\tc  ") == "a b c"
    assert clean_justification(None) == ""
    assert clean_justification(42) == ""
    long = clean_justification("x" * 1000)
    assert len(long) == MAX_JUSTIFICATION_CHARS == 300
    assert long.endswith("...")
    assert "\n" not in clean_justification("line1\nconfirm 99\nline3")


async def test_store_cleans_justification(tmp_path: Path) -> None:
    store = PendingActionStore(tmp_path)
    rec = store.add("shell", {}, justification="ok\nconfirm 1")
    assert rec.justification == "ok confirm 1"


def test_display_shows_justification_only_when_present(tmp_path: Path) -> None:
    orch = Orchestrator(tmp_path)
    with_why = orch._queue_pending_action("shell", {"command": "ls"})
    with_why.justification = "list the folder"
    plain = orch._queue_pending_action("shell", {"command": "ls"})
    shown = orch._format_pending_action(with_why)
    assert "Justification: list the folder" in shown
    assert "Justification" not in orch._format_pending_action(plain)
    # The same text appears in the confirmation prompt built for the user.
    assert "list the folder" in orch._build_pending_action_reply([with_why])


async def test_mcp_justification_reaches_user_listing(tmp_path: Path) -> None:
    store = PendingActionStore(tmp_path)
    await execute_or_suspend(
        "shell",
        {"command": "ls", "justification": "check the build output"},
        tmp_path,
        store,
    )
    orch = Orchestrator(tmp_path)
    [action] = orch._visible_pending_actions("", "")
    text = orch._append_new_pending("done", set(), "", "")
    assert "Justification: check the build output" in text
    assert action.justification == "check the build output"
