"""Data models for Nerv skills."""

from dataclasses import dataclass, field


@dataclass
class SkillDefinition:
    """A reusable skill package loaded from a SKILL.md folder."""

    name: str
    description: str
    tags: list[str] = field(default_factory=list)
    owner_agents: list[str] = field(default_factory=list)
    recommended_workflows: list[str] = field(default_factory=list)
    body: str = ""
    source_path: str = ""
