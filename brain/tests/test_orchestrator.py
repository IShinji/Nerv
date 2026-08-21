"""Tests for the Orchestrator and Agent Registry."""

import tempfile
from pathlib import Path

import pytest
import yaml

from nerv.models import RouteResult
from nerv.orchestrator.orchestrator import Orchestrator
from nerv.orchestrator.registry import AgentDefinition, AgentRegistry
from nerv.tools.registry import ToolResult, registry
from nerv.workflows.models import WorkflowDefinition


class TestAgentRegistry:
    """Test agent loading and matching."""

    def _create_agent_dir(self, tmp: Path, agents: list[dict]) -> Path:
        """Create a temporary agents/ directory with YAML files."""
        agent_dir = tmp / "agents"
        agent_dir.mkdir(parents=True, exist_ok=True)
        for agent_data in agents:
            name = agent_data["name"].lower()
            with open(agent_dir / f"{name}.yaml", "w") as f:
                yaml.dump(agent_data, f)
        return tmp

    def test_loads_agents_from_directory(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = self._create_agent_dir(
                Path(tmp),
                [
                    {"name": "General", "description": "General assistant", "tags": ["general"]},
                    {"name": "Coder", "description": "Code helper", "tags": ["code", "debug"]},
                ],
            )
            registry = AgentRegistry(root)
            assert len(registry.list_agents()) == 2

    def test_find_by_type_exact_match(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = self._create_agent_dir(
                Path(tmp),
                [{"name": "Coder", "description": "Code", "tags": ["code"]}],
            )
            registry = AgentRegistry(root)
            agent = registry.find_by_type("coder")
            assert agent is not None
            assert agent.name == "Coder"

    def test_find_by_type_case_insensitive(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = self._create_agent_dir(
                Path(tmp),
                [{"name": "SysAdmin", "description": "Admin", "tags": ["server"]}],
            )
            registry = AgentRegistry(root)
            assert registry.find_by_type("sysadmin") is not None
            assert registry.find_by_type("SYSADMIN") is not None

    def test_find_by_type_returns_none_for_unknown(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = self._create_agent_dir(
                Path(tmp),
                [{"name": "General", "description": "General", "tags": []}],
            )
            registry = AgentRegistry(root)
            assert registry.find_by_type("unknown_agent") is None

    def test_find_by_tags(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = self._create_agent_dir(
                Path(tmp),
                [
                    {"name": "General", "description": "General", "tags": ["general", "chat"]},
                    {"name": "Coder", "description": "Code", "tags": ["code", "debug", "programming"]},
                ],
            )
            registry = AgentRegistry(root)
            agent = registry.find_by_tags(["debug", "code"])
            assert agent is not None
            assert agent.name == "Coder"

    def test_get_default_returns_general(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = self._create_agent_dir(
                Path(tmp),
                [{"name": "General", "description": "Default", "tags": []}],
            )
            registry = AgentRegistry(root)
            default = registry.get_default()
            assert default.name == "General"

    def test_get_default_creates_fallback_if_no_general(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = self._create_agent_dir(
                Path(tmp),
                [{"name": "Coder", "description": "Code only", "tags": []}],
            )
            registry = AgentRegistry(root)
            default = registry.get_default()
            assert default.name == "General"
            assert default.system_prompt == "You are a helpful assistant. Answer concisely."

    def test_loads_preset_agents_from_project(self) -> None:
        """Test loading the actual preset agents from the project."""
        # Find the real project root
        test_file = Path(__file__).resolve()
        project_root = test_file.parent.parent  # brain/ → project root

        if not (project_root / "agents").exists():
            return  # Skip if not running from project tree

        registry = AgentRegistry(project_root)
        agents = registry.list_agents()
        assert len(agents) >= 5, f"Expected at least 5 preset agents, got {len(agents)}"

        names = {a.name for a in agents}
        assert "General" in names
        assert "Coder" in names
        assert "Researcher" in names
        assert "Writer" in names
        assert "SysAdmin" in names


@pytest.mark.asyncio
async def test_confirm_pending_action_executes_tool(tmp_path: Path) -> None:
    """Test confirming a pending action executes the queued tool."""
    orchestrator = Orchestrator(tmp_path)
    tool_def = registry.get_tool("desktop_control")
    assert tool_def is not None

    original_func = tool_def.func
    tool_def.func = lambda **kwargs: ToolResult(content="Desktop action executed")
    try:
        queued = orchestrator._queue_pending_action(
            "desktop_control",
            {"action": "open_application", "application": "Safari"},
            sender="user-1",
            channel="telegram",
        )

        response = await orchestrator.dispatch(
            f"confirm {queued.id}",
            RouteResult(intent="general"),
            channel="telegram",
            sender="user-1",
        )
    finally:
        tool_def.func = original_func

    assert "Executed pending action" in response
    assert "Desktop action executed" in response
    assert orchestrator._pending_store.list() == []


@pytest.mark.asyncio
async def test_cancel_pending_action_discards_it(tmp_path: Path) -> None:
    """Test cancelling a pending action removes it without execution."""
    orchestrator = Orchestrator(tmp_path)
    queued = orchestrator._queue_pending_action(
        "screenshot",
        {"output_path": "/tmp/capture.png"},
        sender="user-1",
        channel="telegram",
    )

    response = await orchestrator.dispatch(
        f"cancel {queued.id}",
        RouteResult(intent="general"),
        channel="telegram",
        sender="user-1",
    )

    assert f"Cancelled pending action #{queued.id}" in response
    assert orchestrator._pending_store.list() == []


@pytest.mark.asyncio
async def test_approve_workflow_review_publishes_shared_workflow(tmp_path: Path) -> None:
    """Direct approval commands should publish pending workflow proposals."""
    orchestrator = Orchestrator(tmp_path)
    item = orchestrator.review_queue.submit_workflow(
        WorkflowDefinition(
            name="DraftFlow",
            description="Temporary workflow",
            owner_agents=["general"],
            tools=["chrome_browser"],
            steps=["open page", "extract result"],
            created_by="agent",
            review_status="pending",
        ),
        proposed_by="general",
    )

    response = await orchestrator.dispatch(
        f"approve workflow {item.review_id}",
        RouteResult(intent="general"),
    )

    assert "Approved workflow review" in response
    assert orchestrator.workflow_registry.find("DraftFlow") is not None
