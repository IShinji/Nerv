"""Tools for self-evolving agents: evolve, list versions, roll back.

These edit agent YAML under ``personal/agents/`` (in-sandbox), always snapshotting
the prior version first so evolution is auditable and reversible.
"""

from __future__ import annotations

import logging

import yaml

from nerv.config import get_tier_models
from nerv.evolution import AgentVersionStore
from nerv.orchestrator.registry import AgentDefinition
from nerv.tools.builtins import _get_project_root
from nerv.tools.registry import ToolResult, registry

logger = logging.getLogger(__name__)

_EVOLVE_SYSTEM_PROMPT = """\
You evolve an existing AI agent definition. You are given the agent's current
YAML and an update instruction. Return the COMPLETE updated YAML, preserving the
structure and all fields. Typically you revise or extend `system_prompt` to
integrate the new knowledge or behavior. Keep `name` unchanged. Output ONLY raw
YAML, no markdown fences, no commentary.
"""


def _clean_yaml(text: str) -> str:
    text = text.strip()
    if text.startswith("```yaml"):
        text = text[7:]
    elif text.startswith("```"):
        text = text[3:]
    if text.endswith("```"):
        text = text[:-3]
    return text.strip()


@registry.register(
    name="evolve_agent",
    description="Update an existing agent's knowledge/behavior. Snapshots the old "
    "version first. Args: agent_name, update_instruction.",
    requires_confirmation=False,
)
async def evolve_agent(agent_name: str, update_instruction: str) -> ToolResult:
    """Revise an agent's definition per an instruction, with version history."""
    from nerv.llm import complete

    root = _get_project_root()
    store = AgentVersionStore(root)
    file = store.find_file(agent_name)
    if file is None:
        return ToolResult(content=f"No agent named '{agent_name}'.", is_error=True)

    current_yaml = file.read_text(encoding="utf-8")
    messages = [
        {"role": "system", "content": _EVOLVE_SYSTEM_PROMPT},
        {
            "role": "user",
            "content": (
                f"Current agent YAML:\n{current_yaml}\n\n"
                f"Update instruction: {update_instruction}"
            ),
        },
    ]
    result = await complete(
        messages, model=get_tier_models(root)[2], temperature=0.3, project_root=root
    )
    new_yaml = _clean_yaml(result.content)

    # Validate before persisting — never write a broken definition.
    try:
        data = yaml.safe_load(new_yaml)
        AgentDefinition(**data)
    except Exception as exc:
        return ToolResult(
            content=f"Proposed evolution was invalid and was discarded: {exc}",
            is_error=True,
        )

    version = store.snapshot(agent_name, note=update_instruction[:120])
    file.write_text(new_yaml, encoding="utf-8")
    return ToolResult(
        content=(
            f"Evolved agent '{agent_name}'. Previous version saved as {version}. "
            "Takes effect on the next brain reload."
        )
    )


@registry.register(
    name="list_agent_versions",
    description="List the saved version history for an agent. Arg: agent_name.",
    requires_confirmation=False,
)
def list_agent_versions(agent_name: str) -> ToolResult:
    """List archived versions of an agent definition."""
    versions = AgentVersionStore(_get_project_root()).list_versions(agent_name)
    if not versions:
        return ToolResult(content=f"No saved versions for '{agent_name}'.")
    lines = [f"- {v.version}: {v.note or '(no note)'}" for v in versions]
    return ToolResult(content=f"Versions for '{agent_name}':\n" + "\n".join(lines))


@registry.register(
    name="rollback_agent",
    description="Roll an agent back to a saved version. Args: agent_name, version.",
    requires_confirmation=False,
)
def rollback_agent(agent_name: str, version: str) -> ToolResult:
    """Restore an agent to a previously archived version."""
    ok = AgentVersionStore(_get_project_root()).rollback(agent_name, version)
    if not ok:
        return ToolResult(
            content=f"Could not roll back '{agent_name}' to version {version}.",
            is_error=True,
        )
    return ToolResult(
        content=(
            f"Rolled '{agent_name}' back to {version}. Takes effect on next reload."
        )
    )
