"""State files carry a schema_version; newer files are never overwritten."""

from __future__ import annotations

import json
import logging
from pathlib import Path

import pytest

from nerv.security import PendingActionStore
from nerv.state_version import CURRENT_SCHEMA_VERSION
from nerv.tasks.store import TaskRunStore


def _read(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


# --- pending actions -------------------------------------------------------


def test_pending_store_writes_current_version(tmp_path: Path) -> None:
    store = PendingActionStore(tmp_path)
    store.add("shell", {"command": "ls"}, justification="list files")
    data = _read(tmp_path / "personal" / "pending_actions.json")
    assert data["schema_version"] == CURRENT_SCHEMA_VERSION == 1
    assert data["actions"][0]["justification"] == "list files"


def test_pending_store_reads_legacy_file(tmp_path: Path) -> None:
    (tmp_path / "personal").mkdir()
    legacy = {
        "next_id": 4,
        "actions": [
            {
                "id": 3,
                "tool_name": "shell",
                "arguments": {},
                "created_at": 9_999_999_999.0,
            }
        ],
    }
    (tmp_path / "personal" / "pending_actions.json").write_text(json.dumps(legacy))
    store = PendingActionStore(tmp_path)
    [record] = store.list()
    assert record.id == 3
    assert record.justification == ""
    store.add("shell", {})
    data = _read(tmp_path / "personal" / "pending_actions.json")
    assert data["schema_version"] == 1
    assert data["next_id"] == 5


def test_pending_store_does_not_overwrite_newer_file(
    tmp_path: Path, caplog: pytest.LogCaptureFixture
) -> None:
    (tmp_path / "personal").mkdir()
    path = tmp_path / "personal" / "pending_actions.json"
    future = {"schema_version": 99, "next_id": 7, "actions": [{"id": 1}]}
    path.write_text(json.dumps(future))
    store = PendingActionStore(tmp_path)
    with caplog.at_level(logging.ERROR):
        assert store.list() == []
        with pytest.raises(RuntimeError, match="newer"):
            store.add("shell", {})
        assert store.remove(1) is None
    assert _read(path) == future
    assert "schema_version" in caplog.text


# --- task runs -------------------------------------------------------------


def test_task_run_writes_version_and_reads_legacy(tmp_path: Path) -> None:
    store = TaskRunStore(tmp_path)
    run = store.create("wf", "input", ["a", "b"])
    path = tmp_path / "personal" / "tasks" / f"{run.id}.json"
    assert _read(path)["schema_version"] == 1

    legacy = _read(path)
    del legacy["schema_version"]
    path.write_text(json.dumps(legacy))
    loaded = store.get(run.id)
    assert loaded is not None
    assert loaded.schema_version == 1
    store.save(loaded)
    assert _read(path)["schema_version"] == 1


def test_task_run_newer_file_is_not_overwritten(tmp_path: Path) -> None:
    store = TaskRunStore(tmp_path)
    run = store.create("wf", "input", ["a"])
    path = tmp_path / "personal" / "tasks" / f"{run.id}.json"
    future = _read(path)
    future["schema_version"] = 5
    path.write_text(json.dumps(future))
    assert store.get(run.id) is None
    run.status = "done"
    store.save(run)
    assert _read(path) == future
