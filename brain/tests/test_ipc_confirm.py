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


async def test_finish_turn_passes_loop_output_to_the_orchestrator(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """``finish_turn`` hands the loop's reply to the orchestrator and returns
    its finalized response verbatim."""
    received: dict[str, Any] = {}

    class FakeOrchestrator:
        async def finish_turn(
            self,
            message,
            reply,
            channel="",
            sender="",
            known_pending=None,
            notifications=None,
        ):
            received.update(
                message=message,
                reply=reply,
                channel=channel,
                sender=sender,
                known_pending=known_pending,
                notifications=notifications,
            )
            return {"reply": "final reply", "pending_created": True}

    async def get_orch() -> Any:
        return FakeOrchestrator()

    monkeypatch.setattr(ipc, "_get_orchestrator", get_orch)

    result = await ipc.handle_request(
        "finish_turn",
        {
            "message": "rm it",
            "reply": "I tried to run it.",
            "channel": "cli",
            "sender": "dev",
            "known_pending": [{"id": 3}],
            "notifications": ["alert"],
        },
    )
    assert result == {"reply": "final reply", "pending_created": True}
    assert received["message"] == "rm it"
    assert received["reply"] == "I tried to run it."
    assert received["known_pending"] == [{"id": 3}]
    assert received["notifications"] == ["alert"]
