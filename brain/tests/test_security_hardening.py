"""Regression tests for the confirmation gate, sandbox, and pending-action UX."""

import asyncio
import time
from pathlib import Path

import pytest

from nerv.memory.context import ContextManager
from nerv.memory.manager import MemoryManager
from nerv.orchestrator.factory import AgentFactory, normalize_role_name
from nerv.orchestrator.orchestrator import Orchestrator
from nerv.orchestrator.registry import AgentDefinition
from nerv.security import PendingActionStore
from nerv.security.sandbox import confirmation_reason, is_protected_path
from nerv.tools import builtins  # noqa: F401  (registers tools)
from nerv.tools.files import file_io, grep_search, read_file
from nerv.tools.registry import ToolResult, registry
from nerv.tools.system import shell


# --- confirmation gate ------------------------------------------------------
def test_gate_reads_requires_confirmation_flag(tmp_path: Path) -> None:
    assert confirmation_reason("anything", {}, tmp_path) is None
    assert confirmation_reason("anything", {}, tmp_path, requires_confirmation=True)


@pytest.mark.parametrize(
    "name",
    [
        "chrome_browser",
        "browser_interactive",
        "delegate_task",
        "evolve_agent",
        "rollback_agent",
        "shell",
        "send_email",
    ],
)
def test_dangerous_tools_are_flagged(name: str) -> None:
    tool = registry.get_tool(name)
    assert tool is not None and tool.requires_confirmation is True


def test_protected_paths_need_confirmation_even_inside_workspace(
    tmp_path: Path,
) -> None:
    for rel in ("config.yaml", "personal/api_keys.env", "brain/nerv/x.py"):
        args = {"absolute_path": str(tmp_path / rel), "content": "x"}
        assert confirmation_reason("write_file_full", args, tmp_path), rel
    ok = {"absolute_path": str(tmp_path / "notes.txt"), "content": "x"}
    assert confirmation_reason("write_file_full", ok, tmp_path) is None
    assert is_protected_path(str(tmp_path / "personal"), tmp_path)


@pytest.mark.asyncio
async def test_workflow_tool_step_goes_through_gate(tmp_path: Path) -> None:
    from nerv.workflows.executor import WorkflowExecutor

    orch = Orchestrator(tmp_path)
    executor = WorkflowExecutor(orch)
    tool_def = registry.get_tool("chrome_browser")
    assert tool_def is not None
    original = tool_def.func
    called = {"n": 0}

    async def spy(**kwargs):
        called["n"] += 1
        return ToolResult(content="ran")

    tool_def.func = spy
    try:
        out = await executor._call_tool_gated(tool_def, {"action": "open_url"})
    finally:
        tool_def.func = original
    assert "ACTION SUSPENDED" in out
    assert called["n"] == 0
    assert len(orch._pending_store.list()) == 1


# --- file tools -------------------------------------------------------------
def test_secret_files_unreadable(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setenv("NERV_PROJECT_ROOT", str(tmp_path))
    (tmp_path / "personal").mkdir()
    secret = tmp_path / "personal" / "api_keys.env"
    secret.write_text("TOKEN=abc")
    cfg = tmp_path / "config.yaml"
    cfg.write_text("token: abc")

    assert read_file(str(secret)).is_error
    assert read_file(str(cfg)).is_error
    assert file_io("read", str(secret)).is_error
    assert grep_search(str(cfg), "token").is_error
    # Searching the parent must not leak the secret either.
    found = grep_search(str(tmp_path), "abc")
    assert "TOKEN=abc" not in found.content
    assert "token: abc" not in found.content


def test_grep_query_starting_with_dash_is_not_an_option(
    tmp_path: Path, monkeypatch
) -> None:
    monkeypatch.setenv("NERV_PROJECT_ROOT", str(tmp_path))
    target = tmp_path / "a.txt"
    target.write_text("--version here\n")
    result = grep_search(str(target), "--version")
    assert "--version here" in result.content


@pytest.mark.asyncio
async def test_shell_times_out_and_has_no_stdin(monkeypatch) -> None:
    from nerv.tools import system

    monkeypatch.setattr(system, "SHELL_TIMEOUT_SECONDS", 1)
    slow = await shell("sleep 5")
    assert slow.is_error and "timed out" in slow.content
    # stdin is closed, so `cat` returns immediately instead of hanging.
    quick = await asyncio.wait_for(shell("cat"), timeout=5)
    assert "Exit code: 0" in quick.content


# --- role name / factory ----------------------------------------------------
def test_normalize_role_name() -> None:
    assert normalize_role_name("Front End") == "Front_End"
    for bad in ("../evil", "a/b", "..", "", "x.yaml", "a\\b"):
        assert normalize_role_name(bad) is None


@pytest.mark.asyncio
async def test_factory_rejects_traversal(tmp_path: Path) -> None:
    factory = AgentFactory(tmp_path)
    assert await factory.create_agent("../../evil", "hint") is None
    assert not list(tmp_path.rglob("evil*"))


# --- pending store / confirm UX --------------------------------------------
def test_pending_actions_expire(tmp_path: Path) -> None:
    store = PendingActionStore(tmp_path, ttl_seconds=0.05)
    rec = store.add("shell", {"command": "ls"})
    assert store.get(rec.id) is not None
    time.sleep(0.1)
    assert store.list() == []
    assert store.remove(rec.id) is None


@pytest.mark.asyncio
@pytest.mark.parametrize("word", ["yes", "y", "ok", "run", "confirm", "no"])
async def test_bare_words_do_not_act_on_pending(tmp_path: Path, word: str) -> None:
    orch = Orchestrator(tmp_path)
    orch._queue_pending_action("shell", {"command": "ls"}, sender="u", channel="c")
    result = await orch._handle_pending_action_command(word, sender="u", channel="c")
    assert result is None
    assert len(orch._pending_store.list()) == 1


@pytest.mark.asyncio
async def test_unscoped_and_foreign_actions_invisible(tmp_path: Path) -> None:
    orch = Orchestrator(tmp_path)
    mine = orch._queue_pending_action("shell", {}, sender="u", channel="c")
    unscoped = orch._pending_store.add("shell", {}, reason="mcp")
    other = orch._queue_pending_action("shell", {}, sender="v", channel="c")
    visible = {a.id for a in orch._visible_pending_actions("u", "c")}
    assert visible == {mine.id}
    assert unscoped.id not in visible and other.id not in visible
    local = {a.id for a in orch._visible_pending_actions("", "")}
    assert local == {unscoped.id}


@pytest.mark.asyncio
async def test_pending_sync_tool_runs_off_event_loop(tmp_path: Path) -> None:
    import threading

    orch = Orchestrator(tmp_path)
    tool_def = registry.get_tool("screenshot")
    assert tool_def is not None
    original = tool_def.func
    seen: dict[str, int] = {}

    def fake(**kwargs):
        seen["thread"] = threading.get_ident()
        return ToolResult(content="ok")

    tool_def.func = fake
    try:
        rec = orch._queue_pending_action("screenshot", {}, sender="u", channel="c")
        await orch._handle_pending_action_command(
            f"confirm {rec.id}", sender="u", channel="c"
        )
    finally:
        tool_def.func = original
    assert seen["thread"] != threading.get_ident()


# --- small bugs -------------------------------------------------------------
@pytest.mark.asyncio
async def test_execute_workflow_awaits_orchestrator(tmp_path: Path, monkeypatch):
    from nerv.tools import knowledge
    from nerv.workflows.models import WorkflowDefinition

    seen = {}

    class FakeExecutor:
        def __init__(self, orch):
            seen["orch"] = orch

        async def execute(self, workflow, inputs):
            return "done"

    async def fake_get():
        return "ORCH"

    import nerv.ipc
    import nerv.workflows.executor as executor_mod

    monkeypatch.setattr(nerv.ipc, "_get_orchestrator", fake_get)
    monkeypatch.setattr(executor_mod, "WorkflowExecutor", FakeExecutor)
    wf = WorkflowDefinition(name="w", description="d", steps=["web_search x"])
    monkeypatch.setattr(
        "nerv.workflows.registry.WorkflowRegistry.find", lambda self, n: wf
    )
    result = await knowledge.execute_workflow("w", "in")
    assert not result.is_error, result.content
    assert seen["orch"] == "ORCH"


def test_mcp_config_carries_memory_scope(tmp_path: Path) -> None:
    import json

    from nerv.llm.claude_cli import _write_mcp_config
    from nerv.memory.manager import use_scope

    with use_scope("telegram-u1"):
        path = _write_mcp_config(tmp_path)
    try:
        env = json.loads(path.read_text())["mcpServers"]["nerv"]["env"]
    finally:
        path.unlink()
    assert env["NERV_MEMORY_SCOPE"] == "telegram-u1"


@pytest.mark.asyncio
async def test_mcp_call_binds_scope(tmp_path: Path, monkeypatch) -> None:
    from nerv.mcp_server import server as mcp_server
    from nerv.memory.manager import active_scope

    monkeypatch.setenv("NERV_MEMORY_SCOPE", "telegram-u1")
    tool_def = registry.get_tool("current_time")
    assert tool_def is not None
    seen = {}
    original = tool_def.func

    def spy(**kwargs):
        seen["scope"] = active_scope()
        return ToolResult(content="t")

    tool_def.func = spy
    try:
        built = mcp_server.build_server(tmp_path)
        handler = built.request_handlers
        assert handler  # server constructed with the scope-binding handler
        from mcp import types

        req = types.CallToolRequest(
            method="tools/call",
            params=types.CallToolRequestParams(name="current_time", arguments={}),
        )
        await handler[types.CallToolRequest](req)
    finally:
        tool_def.func = original
    assert seen["scope"] == "telegram-u1"


def test_user_message_not_duplicated_in_prompt(tmp_path: Path) -> None:
    manager = MemoryManager(tmp_path)
    manager.append_message("user", "earlier")
    manager.append_message("assistant", "reply")
    manager.append_message("user", "hello now")  # persisted before build
    agent = AgentDefinition(name="A", description="d", system_prompt="s")
    messages = ContextManager(manager).build_messages(agent, "hello now")
    assert [m["role"] for m in messages] == ["system", "user", "assistant", "user"]
    assert [m["content"] for m in messages].count("hello now") == 1
