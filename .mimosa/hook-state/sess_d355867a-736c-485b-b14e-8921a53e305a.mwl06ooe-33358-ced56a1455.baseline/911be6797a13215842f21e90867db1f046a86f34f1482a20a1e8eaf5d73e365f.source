"""Tests for persistent, resumable task-run state."""

from pathlib import Path

from nerv.tasks import TaskRunStore
from nerv.tasks.store import DONE


def test_create_persist_and_reload(tmp_path: Path) -> None:
    store = TaskRunStore(tmp_path)
    run = store.create("demo", "do the thing", ["step one", "step two"])

    assert len(run.steps) == 2
    assert run.first_unfinished().idx == 1

    reloaded = TaskRunStore(tmp_path).get(run.id)
    assert reloaded is not None
    assert reloaded.workflow_name == "demo"
    assert [s.description for s in reloaded.steps] == ["step one", "step two"]


def test_resume_picks_first_unfinished_step(tmp_path: Path) -> None:
    store = TaskRunStore(tmp_path)
    run = store.create("demo", "input", ["a", "b", "c"])

    run.steps[0].status = DONE
    run.steps[0].result = "done a"
    store.save(run)

    reloaded = store.get(run.id)
    assert reloaded.first_unfinished().idx == 2
    assert not reloaded.is_complete()


def test_list_incomplete(tmp_path: Path) -> None:
    store = TaskRunStore(tmp_path)
    running = store.create("running", "x", ["a"])
    done = store.create("done", "y", ["a"])
    done.steps[0].status = DONE
    done.status = "done"
    store.save(done)

    incomplete_ids = {r.id for r in store.list_incomplete()}
    assert running.id in incomplete_ids
    assert done.id not in incomplete_ids
