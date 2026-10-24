"""The native tool loop must stop after a configurable number of rounds."""

import asyncio
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from nerv.config import get_max_tool_iterations
from nerv.orchestrator.orchestrator import Orchestrator


def test_default_cap_is_20(tmp_path: Path) -> None:
    assert get_max_tool_iterations(tmp_path) == 20


async def test_runaway_tool_loop_is_capped(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    orch = Orchestrator(tmp_path)
    monkeypatch.setattr(
        "nerv.orchestrator.orchestrator.get_max_tool_iterations",
        lambda root: 3,
        raising=False,
    )
    calls = 0

    async def forever(messages: Any, **kwargs: Any) -> Any:
        nonlocal calls
        calls += 1
        return SimpleNamespace(
            content="",
            tool_calls=[{"function": {"name": "no_such_tool", "arguments": {}}}],
            runs_own_tool_loop=False,
        )

    monkeypatch.setattr("nerv.llm.complete", forever)

    reply = await asyncio.wait_for(
        orch._call_model("local:x", [{"role": "user", "content": "hi"}]), timeout=5
    )
    assert calls == 3
    assert "3" in reply and "tool" in reply.lower()
