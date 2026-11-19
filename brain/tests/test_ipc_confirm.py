"""The brain's RPC surface after the turn's stages migrated to Rust.

Pending commands, turn preparation and finalization run in Rust
(``core/brain/src/turn/prepare.rs`` — see the tests there for the confirm/
cancel contract). Python keeps the leaf operations: ``execute_pending``
(python-only confirmed tools), ``queue_notifications`` (the heartbeat drain)
and ``workflow_fallback`` (the still-python WorkflowExecutor). This file pins
that leaf contract.
"""

from typing import Any

import pytest

from nerv import ipc


async def test_execute_pending_runs_the_record_without_a_store_round_trip(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """``execute_pending`` rebuilds the record from the RPC payload."""
    received: dict[str, Any] = {}

    class FakeOrchestrator:
        async def execute_pending_record(
            self,
            action_id: int,
            tool_name: str = "",
            arguments: dict[str, Any] | None = None,
            justification: str = "",
        ) -> dict[str, Any]:
            received.update(
                id=action_id,
                tool_name=tool_name,
                arguments=arguments,
                justification=justification,
            )
            return {"content": "ran it"}

    async def get_orch() -> Any:
        return FakeOrchestrator()

    monkeypatch.setattr(ipc, "_get_orchestrator", get_orch)

    result = await ipc.handle_request(
        "execute_pending",
        {
            "id": 3,
            "tool_name": "execute_workflow",
            "arguments": {"workflow_name": "AskExample"},
            "justification": "user approved",
        },
    )
    assert result == {"content": "ran it"}
    assert received["id"] == 3
    assert received["tool_name"] == "execute_workflow"
    assert received["arguments"] == {"workflow_name": "AskExample"}
    assert received["justification"] == "user approved"


async def test_queue_notifications_reaches_the_heartbeat_drain(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Alerts queued by the Rust loop are surfaced by the next heartbeat."""
    class FakeOrchestrator:
        def __init__(self) -> None:
            self.pending: list[str] = []

        def queue_notifications(self, notifications: list[str]) -> None:
            self.pending.extend(notifications)

        async def check_background_tasks(self) -> str | None:
            batch, self.pending = self.pending, []
            return "\n".join(batch) if batch else None

    orch = FakeOrchestrator()

    async def get_orch() -> Any:
        return orch

    monkeypatch.setattr(ipc, "_get_orchestrator", get_orch)

    result = await ipc.handle_request(
        "queue_notifications",
        {"notifications": ["⚠️ pending action #7 (shell) is waiting"]},
    )
    assert result == {"ok": True}

    drained = await ipc.handle_request("heartbeat", {})
    assert "pending action #7" in drained["reply"]

    # A second heartbeat sees an empty queue.
    drained = await ipc.handle_request("heartbeat", {})
    assert drained["reply"] == ""
