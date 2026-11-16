"""Pending confirmations must be resolvable without any model classification.

Since the router migrated to Rust, the brain only exposes ``check_pending``
(confirm/cancel <id>, consulted before any model call) and ``execute``. The
host-side ordering is tested in ``core/brain/src/handle.rs``; here we pin the
brain side of the contract.
"""

from typing import Any

import pytest

from nerv import ipc


async def test_check_pending_resolves_a_confirm_without_a_model_call(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class FakeOrchestrator:
        async def dispatch_pending_command(
            self, message: str, channel: str = "", sender: str = ""
        ) -> str | None:
            return "done" if message == "confirm 3" else None

    async def get_orch() -> Any:
        return FakeOrchestrator()

    monkeypatch.setattr(ipc, "_get_orchestrator", get_orch)

    result = await ipc.handle_request(
        "check_pending", {"message": "confirm 3", "channel": "t", "sender": "u"}
    )
    assert result == {"pending": True, "reply": "done"}


async def test_check_pending_reports_miss_for_normal_messages(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class FakeOrchestrator:
        async def dispatch_pending_command(
            self, message: str, channel: str = "", sender: str = ""
        ) -> str | None:
            return None

    async def get_orch() -> Any:
        return FakeOrchestrator()

    monkeypatch.setattr(ipc, "_get_orchestrator", get_orch)

    result = await ipc.handle_request(
        "check_pending", {"message": "hello", "channel": "t", "sender": "u"}
    )
    assert result == {"pending": False, "reply": ""}


async def test_execute_dispatches_with_the_rust_routing_decision(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """``execute`` carries the host's routing decision into the orchestrator."""
    received: dict[str, Any] = {}

    class FakeOrchestrator:
        async def dispatch(self, message, route_result, channel="", sender=""):
            received["message"] = message
            received["route_result"] = route_result
            received["channel"] = channel
            received["sender"] = sender
            return "agent reply"

    async def get_orch() -> Any:
        return FakeOrchestrator()

    monkeypatch.setattr(ipc, "_get_orchestrator", get_orch)

    result = await ipc.handle_request(
        "execute",
        {
            "message": "help me debug this python function",
            "channel": "cli",
            "sender": "dev",
            "route_result": {
                "intent": "code_generation",
                "agent_type": "coder",
                "model_tier": 1,
            },
        },
    )
    assert result["reply"] == "agent reply"
    assert result["intent"] == "code_generation"
    assert result["model_tier"] == 1  # pydantic default filled in
    assert received["route_result"].agent_type == "coder"
