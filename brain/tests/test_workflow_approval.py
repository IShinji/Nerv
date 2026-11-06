"""A workflow whose steps need confirmation asks for one approval for the whole run.

Before this, every browser step of a workflow was queued as its own pending action
while the run was marked DONE, so a workflow such as "open Gemini, fill the prompt,
submit, read the answer" finished at once with five independent confirmations and
nothing executed. Now the user approves the listed steps once, and the run only
stops when a step needs a confirmation of its own.
"""

from __future__ import annotations

import copy
from pathlib import Path
from typing import Any

import pytest

from nerv.llm.client import CompletionResult
from nerv.orchestrator.orchestrator import Orchestrator
from nerv.security import PendingRecord
from nerv.security.confirmed import confirmed_action, executing_confirmed
from nerv.tasks import TaskRunStore
from nerv.tasks.store import DONE, PENDING, SUSPENDED
from nerv.tools import knowledge
from nerv.tools.builtins import registry
from nerv.tools.registry import ToolResult
from nerv.workflows import executor as executor_mod
from nerv.workflows.executor import (
    SuspendedText,
    WorkflowExecutor,
    WorkflowNeedsApproval,
)
from nerv.workflows.models import WorkflowDefinition

_BROWSER_STEPS = [
    "chrome_browser.open_url -> {url}",
    "chrome_browser.wait_for_idle",
    "chrome_browser.fill_prompt -> %inputs%",
    "chrome_browser.submit_prompt",
    "chrome_browser.get_page_text",
]


def _browser_workflow(url: str = "https://example.org") -> WorkflowDefinition:
    return WorkflowDefinition(
        name="AskExample",
        description="d",
        tags=["browser"],
        steps=[step.format(url=url) for step in _BROWSER_STEPS],
    )


@pytest.fixture
def browser_calls(monkeypatch: pytest.MonkeyPatch) -> list[dict[str, Any]]:
    """Stand in for the real browser tool (nothing is driven) and record its calls."""
    tool_def = registry.get_tool("chrome_browser")
    assert tool_def is not None
    calls: list[dict[str, Any]] = []

    async def spy(**kwargs: Any) -> ToolResult:
        calls.append(kwargs)
        return ToolResult(content=f"ran {kwargs.get('action')}")

    monkeypatch.setattr(tool_def, "func", spy)
    return calls


def _serve(
    monkeypatch: pytest.MonkeyPatch, orch: Orchestrator, workflow: WorkflowDefinition
) -> None:
    """Make the execute_workflow tool find `workflow` and run it on `orch`."""
    import nerv.ipc

    async def get_orchestrator() -> Orchestrator:
        return orch

    monkeypatch.setattr(nerv.ipc, "_get_orchestrator", get_orchestrator)
    monkeypatch.setattr(
        "nerv.workflows.registry.WorkflowRegistry.find", lambda self, name: workflow
    )


@pytest.mark.asyncio
async def test_gated_workflow_asks_for_one_approval(
    tmp_path: Path, browser_calls: list[dict[str, Any]]
) -> None:
    orch = Orchestrator(tmp_path)

    with pytest.raises(WorkflowNeedsApproval) as exc:
        await WorkflowExecutor(orch).execute(_browser_workflow(), "what is 2+2?")

    pending = orch._pending_store.list()
    assert len(pending) == 1  # one approval, not one per step
    record = pending[0]
    assert record.tool_name == "execute_workflow"
    assert record.arguments == {
        "workflow_name": "AskExample",
        "inputs": "what is 2+2?",
    }
    assert browser_calls == []  # nothing ran
    assert not (tmp_path / "personal" / "tasks").exists()  # and no run was recorded
    # The user is shown what they are approving.
    assert f"confirm {record.id}" in exc.value.message
    for shown in ("https://example.org", "what is 2+2?", "get_page_text"):
        assert shown in record.reason
    assert not exc.value.stale


@pytest.mark.asyncio
async def test_execute_workflow_tool_reports_the_pending_approval(
    tmp_path: Path, browser_calls: list[dict[str, Any]], monkeypatch: pytest.MonkeyPatch
) -> None:
    orch = Orchestrator(tmp_path)
    _serve(monkeypatch, orch, _browser_workflow())

    result = await knowledge.execute_workflow("AskExample", "q")

    assert not result.is_error
    assert "ACTION SUSPENDED" in result.content and "NOT executed" in result.content
    assert len(orch._pending_store.list()) == 1
    assert browser_calls == []


@pytest.mark.asyncio
async def test_confirming_runs_every_step_without_more_prompts(
    tmp_path: Path, browser_calls: list[dict[str, Any]], monkeypatch: pytest.MonkeyPatch
) -> None:
    orch = Orchestrator(tmp_path)
    workflow = _browser_workflow()
    _serve(monkeypatch, orch, workflow)
    with pytest.raises(WorkflowNeedsApproval):
        await WorkflowExecutor(orch).execute(workflow, "q")
    record = orch._pending_store.list()[0]

    reply = await orch._execute_pending_action(record)

    assert [c["action"] for c in browser_calls] == [
        "open_url",
        "wait_for_idle",
        "fill_prompt",
        "submit_prompt",
        "get_page_text",
    ]
    assert browser_calls[0]["url"] == "https://example.org"
    assert browser_calls[2]["text"] == "q"
    assert orch._pending_store.list() == []  # no further confirmation was needed
    assert reply.startswith("Executed pending action")
    assert "ran get_page_text" in reply
    (path,) = (tmp_path / "personal" / "tasks").glob("*.json")
    run = TaskRunStore(tmp_path).get(path.stem)
    assert run is not None and run.is_complete()


@pytest.mark.asyncio
async def test_approval_does_not_cover_a_changed_workflow(
    tmp_path: Path, browser_calls: list[dict[str, Any]], monkeypatch: pytest.MonkeyPatch
) -> None:
    orch = Orchestrator(tmp_path)
    _serve(monkeypatch, orch, _browser_workflow())
    with pytest.raises(WorkflowNeedsApproval):
        await WorkflowExecutor(orch).execute(_browser_workflow(), "q")
    record = orch._pending_store.list()[0]

    # The workflow is edited after the user was asked, before they confirmed.
    _serve(monkeypatch, orch, _browser_workflow(url="https://example.net"))
    reply = await orch._execute_pending_action(record)

    assert browser_calls == []  # nothing ran on the old approval
    assert reply.startswith("Execution failed for pending action")
    (fresh,) = orch._pending_store.list()  # the user is asked again, with the new plan
    assert fresh.id != record.id
    assert "https://example.net" in fresh.reason


@pytest.mark.asyncio
async def test_approval_is_tied_to_the_inputs(
    tmp_path: Path, browser_calls: list[dict[str, Any]]
) -> None:
    orch = Orchestrator(tmp_path)
    approved = PendingRecord(
        id=7,
        tool_name="execute_workflow",
        arguments={"workflow_name": "AskExample", "inputs": "approved question"},
    )

    with executing_confirmed(approved), pytest.raises(WorkflowNeedsApproval):
        await WorkflowExecutor(orch).execute(_browser_workflow(), "another question")

    assert browser_calls == []


@pytest.mark.asyncio
async def test_confirming_something_else_does_not_approve_the_workflow(
    tmp_path: Path, browser_calls: list[dict[str, Any]]
) -> None:
    orch = Orchestrator(tmp_path)
    other = PendingRecord(id=99, tool_name="shell", arguments={"command": "ls"})

    with executing_confirmed(other), pytest.raises(WorkflowNeedsApproval):
        await WorkflowExecutor(orch).execute(_browser_workflow(), "q")

    assert browser_calls == []


@pytest.mark.asyncio
async def test_run_stops_at_a_step_that_needs_its_own_confirmation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    calls = {"n": 0}

    async def fake_complete(messages: list[dict[str, Any]], **kwargs: Any):
        calls["n"] += 1
        content = '{"command": "echo hi"}' if kwargs.get("json_mode") else "summary"
        return CompletionResult(content=content)

    monkeypatch.setattr(executor_mod, "complete", fake_complete)
    orch = Orchestrator(tmp_path)
    workflow = WorkflowDefinition(
        name="Mixed",
        description="d",
        # The shell command is chosen by the model while the run is going, so it
        # cannot be listed for approval up front.
        steps=["Summarize the input", "shell echo hi", "Write the final answer"],
    )

    with pytest.raises(WorkflowNeedsApproval) as exc:
        await WorkflowExecutor(orch).execute(workflow, "x")

    pending = orch._pending_store.list()
    assert [p.tool_name for p in pending] == ["shell"]  # queued once, never retried
    assert "step 2/3" in exc.value.message
    assert f"confirm {pending[0].id}" in exc.value.message
    assert calls["n"] == 2  # the last step did not run
    (path,) = (tmp_path / "personal" / "tasks").glob("*.json")
    run = TaskRunStore(tmp_path).get(path.stem)
    assert run is not None
    assert run.status == SUSPENDED  # not "done"
    assert [s.status for s in run.steps] == [DONE, SUSPENDED, PENDING]


@pytest.mark.asyncio
async def test_fallback_workflow_asks_instead_of_dropping_to_a_weaker_model(
    tmp_path: Path, browser_calls: list[dict[str, Any]], monkeypatch: pytest.MonkeyPatch
) -> None:
    orch = Orchestrator(tmp_path)
    workflow = _browser_workflow()

    async def unavailable(_model: str) -> bool:
        return False

    monkeypatch.setattr(orch, "_model_available", unavailable)
    monkeypatch.setattr(
        orch.workflow_registry, "find_for_agent", lambda agent: [workflow]
    )

    reply = await orch._try_workflow_fallback(object(), "q", 0, "local:missing")

    assert reply is not None
    (record,) = orch._pending_store.list()
    assert f"confirm {record.id}" in reply
    assert "ACTION SUSPENDED" not in reply  # the user-facing text, not the model's
    assert browser_calls == []


def test_suspended_text_survives_copying() -> None:
    # Saving a run deep-copies step results (dataclasses.asdict).
    copied = copy.deepcopy(SuspendedText("queued", 3))

    assert copied == "queued"
    assert copied.pending_id == 3


def test_confirmed_action_is_only_set_inside_the_block() -> None:
    record = PendingRecord(id=1, tool_name="shell", arguments={})
    assert confirmed_action() is None
    with executing_confirmed(record):
        assert confirmed_action() is record
    assert confirmed_action() is None
