"""Tests for self-evolving agents: snapshot, list, rollback, and evolve."""

from pathlib import Path

import pytest

from nerv.evolution import AgentVersionStore
from nerv.llm.client import CompletionResult
from nerv.tools import evolution_tools

AGENT_V1 = """\
name: "TaxAdvisor"
description: "Helps with taxes"
system_prompt: |
  You are a tax advisor. Rules for 2025.
tools: []
"""

AGENT_V2 = """\
name: "TaxAdvisor"
description: "Helps with taxes"
system_prompt: |
  You are a tax advisor. Rules for 2026.
tools: []
"""


def _make_agent(root: Path, body: str = AGENT_V1) -> Path:
    agents = root / "personal" / "agents"
    agents.mkdir(parents=True, exist_ok=True)
    path = agents / "taxadvisor.yaml"
    path.write_text(body, encoding="utf-8")
    return path


def test_snapshot_list_and_rollback(tmp_path: Path) -> None:
    path = _make_agent(tmp_path)
    store = AgentVersionStore(tmp_path)

    version = store.snapshot("TaxAdvisor", note="initial")
    assert version is not None

    # Mutate the live file, then roll back to the snapshot.
    path.write_text(AGENT_V2, encoding="utf-8")
    assert "2026" in path.read_text()

    versions = store.list_versions("TaxAdvisor")
    assert any(v.note == "initial" for v in versions)

    assert store.rollback("TaxAdvisor", version) is True
    assert "2025" in path.read_text()


def test_find_file_by_name_field(tmp_path: Path) -> None:
    _make_agent(tmp_path)
    store = AgentVersionStore(tmp_path)
    # Case-insensitive match on the `name:` field.
    assert store.find_file("taxadvisor") is not None
    assert store.find_file("TAXADVISOR") is not None
    assert store.find_file("missing") is None


@pytest.mark.asyncio
async def test_evolve_agent_snapshots_then_writes(tmp_path: Path, monkeypatch) -> None:
    path = _make_agent(tmp_path)
    monkeypatch.setattr(evolution_tools, "_get_project_root", lambda: tmp_path)

    async def fake_complete(messages, **kwargs):
        return CompletionResult(content=AGENT_V2)

    monkeypatch.setattr("nerv.llm.complete", fake_complete)

    result = await evolution_tools.evolve_agent("TaxAdvisor", "update for 2026 rules")
    assert not result.is_error
    assert "2026" in path.read_text()
    # The pre-evolution version was archived.
    assert AgentVersionStore(tmp_path).list_versions("TaxAdvisor")


@pytest.mark.asyncio
async def test_evolve_rejects_invalid_yaml(tmp_path: Path, monkeypatch) -> None:
    path = _make_agent(tmp_path)
    monkeypatch.setattr(evolution_tools, "_get_project_root", lambda: tmp_path)

    async def bad_complete(messages, **kwargs):
        return CompletionResult(content="name: [unclosed")

    monkeypatch.setattr("nerv.llm.complete", bad_complete)

    result = await evolution_tools.evolve_agent("TaxAdvisor", "break it")
    assert result.is_error
    # Live file is untouched on invalid output.
    assert "2025" in path.read_text()
