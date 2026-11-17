"""Registry for SKILL.md-based capability packs."""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING

import yaml

from nerv.skills.models import SkillDefinition

if TYPE_CHECKING:
    from nerv.orchestrator.registry import AgentDefinition


class SkillRegistry:
    """Load reusable skill packages from SKILL.md folders."""

    def __init__(self, project_root: Path) -> None:
        self.project_root = project_root
        self.skills_dir = project_root / "skills"
        self._skills: dict[str, SkillDefinition] = {}
        self.reload()

    def reload(self) -> None:
        """Reload skills from disk."""
        self._skills = {}
        if not self.skills_dir.exists():
            return

        for path in sorted(self.skills_dir.glob("*/SKILL.md")):
            skill = self._parse_skill_file(path)
            self._skills[skill.name.lower()] = skill

    def list_skills(self) -> list[SkillDefinition]:
        """Return all loaded skills."""
        return list(self._skills.values())

    def find(self, name: str) -> SkillDefinition | None:
        """Find a skill by name."""
        return self._skills.get(name.strip().lower())

    def find_for_agent(self, agent: AgentDefinition) -> list[SkillDefinition]:
        """Return skills relevant to an agent."""
        agent_name = agent.name.strip().lower()
        agent_tags = {tag.lower() for tag in agent.tags}

        matches: list[SkillDefinition] = []
        for skill in self._skills.values():
            owners = {owner.lower() for owner in skill.owner_agents}
            tags = {tag.lower() for tag in skill.tags}
            if agent_name in owners or agent_tags.intersection(tags):
                matches.append(skill)

        return matches

    def render_catalog(self, agent: AgentDefinition, max_items: int = 4) -> str:
        """Render a compact skill summary for prompt injection."""
        skills = self.find_for_agent(agent)
        if not skills:
            return ""

        lines = ["[Skill Library]"]
        for skill in skills[:max_items]:
            lines.append(f"- {skill.name}: {skill.description}")
            if skill.recommended_workflows:
                lines.append(
                    "  Recommended workflows: "
                    + ", ".join(skill.recommended_workflows[:4])
                )
        lines.append(
            "- Skills are reusable guidance packs. Use them to choose or design workflows more consistently."
        )
        return "\n".join(lines)

    def _parse_skill_file(self, path: Path) -> SkillDefinition:
        """Parse a SKILL.md file with optional YAML frontmatter."""
        raw = path.read_text(encoding="utf-8")
        metadata: dict[str, object] = {}
        body = raw.strip()

        if raw.startswith("---\n"):
            _, frontmatter, rest = raw.split("---\n", 2)
            metadata = yaml.safe_load(frontmatter) or {}
            body = rest.strip()

        return SkillDefinition(
            name=str(metadata.get("name", path.parent.name)),
            description=str(metadata.get("description", "")),
            tags=list(metadata.get("tags", [])),
            owner_agents=list(metadata.get("owner_agents", [])),
            recommended_workflows=list(metadata.get("recommended_workflows", [])),
            body=body,
            source_path=str(path),
        )
