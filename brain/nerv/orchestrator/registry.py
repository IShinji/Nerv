"""Agent registry — loads and matches agent definitions from YAML files.

Searches for agents in two locations (priority order):
1. personal/agents/  — user-created and auto-created agents
2. agents/           — preset agents shipped with the system
"""

import logging
from dataclasses import dataclass, field
from pathlib import Path

import yaml

logger = logging.getLogger(__name__)


@dataclass
class AgentDefinition:
    """Parsed agent definition from a YAML file."""

    name: str
    description: str
    tags: list[str] = field(default_factory=list)
    model_tier: list[int] = field(default_factory=lambda: [0])
    system_prompt: str = ""
    tools: list[str] = field(default_factory=list)
    max_context_tokens: int = 4000
    summary_strategy: str = "rolling"
    created_by: str = "preset"
    source_path: str = ""


class AgentRegistry:
    """Registry that loads and matches agents from YAML definitions."""

    def __init__(self, project_root: Path) -> None:
        self._agents: dict[str, AgentDefinition] = {}
        self._project_root = project_root
        self._load_agents()

    def _load_agents(self) -> None:
        """Load agent definitions from disk."""
        # Load preset agents first, then personal (personal overrides preset)
        preset_dir = self._project_root / "agents"
        personal_dir = self._project_root / "personal" / "agents"

        for agent_dir in [preset_dir, personal_dir]:
            if not agent_dir.exists():
                continue
            for yaml_file in sorted(agent_dir.glob("*.yaml")):
                try:
                    agent = self._parse_agent_file(yaml_file)
                    self._agents[agent.name.lower()] = agent
                    logger.debug("Loaded agent: %s from %s", agent.name, yaml_file)
                except Exception:
                    logger.exception("Failed to load agent from %s", yaml_file)

        logger.info("Agent registry loaded: %d agents", len(self._agents))

    def _parse_agent_file(self, path: Path) -> AgentDefinition:
        """Parse a single YAML agent definition file."""
        with open(path) as f:
            data = yaml.safe_load(f)

        return AgentDefinition(
            name=data["name"],
            description=data.get("description", ""),
            tags=data.get("tags", []),
            model_tier=data.get("model_tier", [0]),
            system_prompt=data.get("system_prompt", ""),
            tools=data.get("tools", []),
            max_context_tokens=data.get("max_context_tokens", 4000),
            summary_strategy=data.get("summary_strategy", "rolling"),
            created_by=data.get("created_by", "preset"),
            source_path=str(path),
        )

    def find_by_type(self, agent_type: str) -> AgentDefinition | None:
        """Find an agent by exact type name match."""
        return self._agents.get(agent_type.lower())

    def find_by_tags(self, tags: list[str]) -> AgentDefinition | None:
        """Find the best matching agent by tag overlap."""
        if not tags:
            return None

        best_match: AgentDefinition | None = None
        best_score = 0

        tag_set = set(t.lower() for t in tags)
        for agent in self._agents.values():
            agent_tags = set(t.lower() for t in agent.tags)
            score = len(tag_set & agent_tags)
            if score > best_score:
                best_score = score
                best_match = agent

        return best_match

    def get_default(self) -> AgentDefinition:
        """Return the general/fallback agent."""
        default = self._agents.get("general")
        if default:
            return default

        # If no general agent exists, create a minimal one
        return AgentDefinition(
            name="General",
            description="Fallback general assistant",
            system_prompt="You are a helpful assistant. Answer concisely.",
        )

    def list_agents(self) -> list[AgentDefinition]:
        """Return all loaded agents."""
        return list(self._agents.values())
