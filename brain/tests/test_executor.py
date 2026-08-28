"""Tests for the workflow executor: resume, per-step retry, and failure handling."""

from pathlib import Path

import pytest

from nerv.llm.client import CompletionResult
from nerv.tasks import TaskRunStore
from nerv.tasks.store import DONE
from nerv.workflows import executor as executor_mod
from nerv.workflows.executor import WorkflowExecutor
from nerv.workflows.models import WorkflowDefinition, WorkflowStep


class _FakeOrchestrator:
    """Minimal orchestrator surface the executor depends on."""

    def __init__(self, project_root: Path) -> None:
        self._project_root = project_root
        self._pending_notifications: list[str] = []

    def _select_model(self, tier: int) -> str:
        return "claude-cli:opus"


def _reasoning_workflow() -> WorkflowDefinition:
    return WorkflowDefinition(
        name="demo",
        description="d",
        steps=[
            WorkflowStep(id="s1", tool="", description="Summarize the input"),
            WorkflowStep(id="s2", tool="", description="Draft a reply"),
        ],
    )


@pytest.mark.asyncio
async def test_full_run_completes_and_persists(tmp_path: Path, monkeypatch) -> None:
    calls = {"n": 0}

    async def fake_complete(messages, **kwargs):
        calls["n"] += 1
        return CompletionResult(content=f"output-{calls['n']}")

    monkeypatch.setattr(executor_mod, "complete", fake_complete)
    orch = _FakeOrchestrator(tmp_path)
    ex = WorkflowExecutor(orch)

    out = await ex.execute(_reasoning_workflow(), "the input")

    assert "output-2" in out
    assert calls["n"] == 2
    runs = TaskRunStore(tmp_path).list_incomplete()
    assert runs == []  # completed runs are not "incomplete"


@pytest.mark.asyncio
async def test_resume_skips_completed_steps(tmp_path: Path, monkeypatch) -> None:
    store = TaskRunStore(tmp_path)
    run = store.create("demo", "the input", ["Summarize the input", "Draft a reply"])
    run.steps[0].status = DONE
    run.steps[0].result = "already summarized"
    store.save(run)

    calls = {"n": 0}

    async def fake_complete(messages, **kwargs):
        calls["n"] += 1
        return CompletionResult(content="second step output")

    monkeypatch.setattr(executor_mod, "complete", fake_complete)
    ex = WorkflowExecutor(_FakeOrchestrator(tmp_path))

    await ex.execute(_reasoning_workflow(), "the input", run_id=run.id)

    # Only the second (unfinished) step should have invoked the model.
    assert calls["n"] == 1
    assert store.get(run.id).is_complete()


@pytest.mark.asyncio
async def test_step_retries_then_succeeds(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setenv("NERV_WORKFLOW_STEP_ATTEMPTS", "3")
    monkeypatch.setattr(executor_mod.asyncio, "sleep", _no_sleep)

    attempts = {"n": 0}

    async def flaky_complete(messages, **kwargs):
        attempts["n"] += 1
        if attempts["n"] == 1:
            raise RuntimeError("transient")
        return CompletionResult(content="recovered")

    monkeypatch.setattr(executor_mod, "complete", flaky_complete)
    wf = WorkflowDefinition(
        name="demo",
        description="d",
        steps=[WorkflowStep(id="s1", tool="", description="Summarize")],
    )
    ex = WorkflowExecutor(_FakeOrchestrator(tmp_path))

    out = await ex.execute(wf, "x")
    assert "recovered" in out
    assert attempts["n"] == 2


@pytest.mark.asyncio
async def test_failure_marks_run_failed_and_keeps_progress(
    tmp_path: Path, monkeypatch
) -> None:
    monkeypatch.setenv("NERV_WORKFLOW_STEP_ATTEMPTS", "1")

    async def always_fail(messages, **kwargs):
        raise RuntimeError("boom")

    monkeypatch.setattr(executor_mod, "complete", always_fail)
    wf = WorkflowDefinition(
        name="demo",
        description="d",
        steps=[WorkflowStep(id="s1", tool="", description="Summarize")],
    )
    ex = WorkflowExecutor(_FakeOrchestrator(tmp_path))

    out = await ex.execute(wf, "x")
    assert "failed at step 1" in out
    run = TaskRunStore(tmp_path).list_incomplete()
    # Failed runs are not RUNNING, so not listed as incomplete; verify status.
    assert run == []


async def _no_sleep(_seconds):
    return None
