"""One heartbeat must deliver every queued notification, batched in one reply."""

from pathlib import Path

from nerv.orchestrator.orchestrator import Orchestrator


async def test_heartbeat_drains_all_notifications(tmp_path: Path) -> None:
    orch = Orchestrator(tmp_path)
    orch._pending_notifications.extend(["first", "second", "third"])

    reply = await orch.check_background_tasks()

    assert reply is not None
    assert reply.index("first") < reply.index("second") < reply.index("third")
    assert orch._pending_notifications == []
    assert await orch.check_background_tasks() is None
