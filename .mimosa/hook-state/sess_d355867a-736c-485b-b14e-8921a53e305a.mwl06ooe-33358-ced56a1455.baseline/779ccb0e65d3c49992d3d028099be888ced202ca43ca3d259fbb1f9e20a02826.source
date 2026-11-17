"""A manufactured agent must be hot-registered without crashing the turn."""

import dataclasses
from pathlib import Path
from typing import Any

import pytest

from nerv.models import RouteResult
from nerv.orchestrator.factory import AgentFactory
from nerv.orchestrator.orchestrator import Orchestrator


async def test_manufactured_agent_is_registered(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    orch = Orchestrator(tmp_path)
    default = orch.registry.get_default()
    new_agent = dataclasses.replace(default, name="Brandnew")

    async def fake_create(self: Any, agent_type: str, hint: str) -> Any:
        return new_agent

    monkeypatch.setattr(AgentFactory, "create_agent", fake_create)

    found = await orch._find_agent(RouteResult(intent="x", agent_type="zzz"), "hi")
    assert found.name == "Brandnew"
    assert any(a.name == "Brandnew" for a in orch.registry.list_agents())
