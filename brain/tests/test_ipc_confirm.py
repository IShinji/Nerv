"""Confirm/cancel commands must not depend on a model classification call."""

from typing import Any

import pytest

from nerv import ipc


async def test_confirm_command_skips_router_classification(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class ExplodingRouter:
        async def classify(self, message: str) -> Any:
            raise AssertionError("router.classify must not run for confirm <id>")

    class FakeOrchestrator:
        async def dispatch_pending_command(
            self, message: str, channel: str = "", sender: str = ""
        ) -> str | None:
            return "done" if message == "confirm 3" else None

    async def get_router() -> Any:
        return ExplodingRouter()

    async def get_orch() -> Any:
        return FakeOrchestrator()

    monkeypatch.setattr(ipc, "_get_router", get_router)
    monkeypatch.setattr(ipc, "_get_orchestrator", get_orch)

    result = await ipc.handle_request(
        "route", {"message": "confirm 3", "channel": "t", "sender": "u"}
    )
    assert result["reply"] == "done"
