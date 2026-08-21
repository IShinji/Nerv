"""Tests for the file-backed pending-action store (cross-process queue)."""

from pathlib import Path

from nerv.security import PendingActionStore


def test_add_assigns_incrementing_ids_and_persists(tmp_path: Path) -> None:
    store = PendingActionStore(tmp_path)
    first = store.add("shell", {"command": "ls"}, reason="risky")
    second = store.add("write_file_full", {"absolute_path": "/tmp/x"})

    assert first.id == 1
    assert second.id == 2
    assert first.reason == "risky"

    # A fresh instance (simulating the other process) sees the same queue.
    reopened = PendingActionStore(tmp_path)
    ids = {rec.id for rec in reopened.list()}
    assert ids == {1, 2}


def test_get_and_remove(tmp_path: Path) -> None:
    store = PendingActionStore(tmp_path)
    rec = store.add("shell", {"command": "ls"})

    assert store.get(rec.id).tool_name == "shell"
    removed = store.remove(rec.id)
    assert removed.id == rec.id
    assert store.get(rec.id) is None
    assert store.remove(999) is None
