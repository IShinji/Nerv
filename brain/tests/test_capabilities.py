"""Tests for capability abstraction and resolution."""

from pathlib import Path

from nerv.capabilities import capability_registry
from nerv.orchestrator.orchestrator import Orchestrator
from nerv.orchestrator.registry import AgentDefinition
from nerv.orchestrator.runtime_policy import build_system_prompt
from nerv.workflows.executor import WorkflowExecutor


def test_capability_registry_resolves_alias_to_native_tool() -> None:
    """Capability aliases should resolve to their concrete provider tools."""
    resolution = capability_registry.resolve("browser.interactive")
    assert resolution is not None
    assert resolution.requested_name == "browser.interactive"
    assert resolution.provider_name == "browser_interactive"
    assert resolution.is_capability is True


def test_capability_registry_get_schemas_deduplicates_same_provider() -> None:
    """Capability aliases should not duplicate schemas for the same provider."""
    schemas = capability_registry.get_schemas(
        ["browser.interactive", "chrome_browser", "browser.read"]
    )
    names = [schema["function"]["name"] for schema in schemas]
    assert names == ["browser_interactive", "chrome_browser", "browser"]


def test_runtime_policy_mentions_capability_aliases() -> None:
    """System prompt should describe capability aliases in a user-facing way."""
    agent = AgentDefinition(
        name="Researcher",
        description="Research helper",
        system_prompt="Use the web carefully.",
        tools=["browser.interactive", "browser.read", "local.exec"],
    )

    system_prompt = build_system_prompt(agent)
    assert "browser.interactive for interactive website tasks" in system_prompt
    assert "browser.read / browser" in system_prompt
    assert "local.exec / shell" in system_prompt


def test_workflow_executor_parses_dotted_capability_step(tmp_path: Path) -> None:
    """Workflow parsing should support dotted capability names."""
    executor = WorkflowExecutor(Orchestrator(tmp_path))

    tool_name, method_hint, args_hint = executor._parse_step(
        "browser.interactive.open_url -> https://example.com"
    )

    assert tool_name == "browser.interactive"
    assert method_hint == "open_url"
    assert args_hint == "https://example.com"
