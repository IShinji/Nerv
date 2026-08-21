"""Persistent task-run state so multi-step workflows survive crashes/restarts.

Each workflow execution is a ``TaskRun`` persisted as JSON under
``personal/tasks/<run_id>.json``. Step results are written after every step, so
an interrupted run can be resumed from the first step that is not yet done.
"""

from __future__ import annotations

import json
import time
import uuid
from dataclasses import asdict, dataclass, field
from pathlib import Path

# Step / run status values.
PENDING = "pending"
RUNNING = "running"
DONE = "done"
FAILED = "failed"


@dataclass
class StepState:
    """Execution state for a single workflow step."""

    idx: int
    description: str
    status: str = PENDING
    result: str = ""
    attempts: int = 0


@dataclass
class TaskRun:
    """A resumable multi-step workflow execution."""

    id: str
    workflow_name: str
    initial_input: str
    status: str = RUNNING
    steps: list[StepState] = field(default_factory=list)
    created_at: float = field(default_factory=time.time)
    updated_at: float = field(default_factory=time.time)

    def first_unfinished(self) -> StepState | None:
        """Return the earliest step that still needs to run."""
        return next((s for s in self.steps if s.status != DONE), None)

    def is_complete(self) -> bool:
        return all(s.status == DONE for s in self.steps)


class TaskRunStore:
    """Filesystem-backed persistence for task runs."""

    def __init__(self, project_root: Path) -> None:
        self._dir = project_root / "personal" / "tasks"

    def _path(self, run_id: str) -> Path:
        return self._dir / f"{run_id}.json"

    def create(
        self, workflow_name: str, initial_input: str, step_descriptions: list[str]
    ) -> TaskRun:
        """Create and persist a fresh run for a workflow's steps."""
        run = TaskRun(
            id=uuid.uuid4().hex[:12],
            workflow_name=workflow_name,
            initial_input=initial_input,
            steps=[
                StepState(idx=i, description=desc)
                for i, desc in enumerate(step_descriptions, start=1)
            ],
        )
        self.save(run)
        return run

    def save(self, run: TaskRun) -> None:
        """Atomically persist the current run state."""
        run.updated_at = time.time()
        self._dir.mkdir(parents=True, exist_ok=True)
        tmp = self._path(run.id).with_suffix(".json.tmp")
        tmp.write_text(
            json.dumps(asdict(run), ensure_ascii=False, indent=2), encoding="utf-8"
        )
        tmp.replace(self._path(run.id))

    def get(self, run_id: str) -> TaskRun | None:
        """Load a run by id, or ``None`` if absent/corrupt."""
        path = self._path(run_id)
        if not path.exists():
            return None
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            return None
        data["steps"] = [StepState(**s) for s in data.get("steps", [])]
        return TaskRun(**data)

    def list_incomplete(self) -> list[TaskRun]:
        """Return all runs that have not finished (for resume on startup)."""
        if not self._dir.exists():
            return []
        runs: list[TaskRun] = []
        for path in self._dir.glob("*.json"):
            run = self.get(path.stem)
            if run is not None and run.status == RUNNING:
                runs.append(run)
        return runs
