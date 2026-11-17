"""Tests for workflow, skill, and review queue registries."""

from pathlib import Path

from nerv.orchestrator.registry import AgentDefinition
from nerv.skills.registry import SkillRegistry
from nerv.workflows.models import WorkflowDefinition
from nerv.workflows.registry import ReviewQueue, WorkflowRegistry


def test_workflow_registry_loads_shared_workflows(tmp_path: Path) -> None:
    """Approved workflows should load from the shared library."""
    workflows_dir = tmp_path / "workflows"
    workflows_dir.mkdir(parents=True, exist_ok=True)
    workflows_dir.joinpath("ask_gemini.yaml").write_text(
        """
name: "AskGeminiAndReturnAnswer"
description: "Ask Gemini and return the answer."
tags: ["research", "browser"]
owner_agents: ["researcher"]
tools: ["chrome_browser"]
steps:
  - "open url"
  - "extract answer"
review_status: "approved"
""".strip(),
        encoding="utf-8",
    )

    registry = WorkflowRegistry(tmp_path)
    agent = AgentDefinition(
        name="Researcher",
        description="",
        tags=["research"],
        system_prompt="",
    )

    workflows = registry.find_for_agent(agent)
    assert len(workflows) == 1
    assert workflows[0].name == "AskGeminiAndReturnAnswer"
    catalog = registry.render_catalog(agent)
    assert "[Workflow Library]" in catalog
    assert "AskGeminiAndReturnAnswer" in catalog


def test_skill_registry_loads_frontmatter_skills(tmp_path: Path) -> None:
    """Skills should load from SKILL.md folders with metadata."""
    skill_dir = tmp_path / "skills" / "browser_operator"
    skill_dir.mkdir(parents=True, exist_ok=True)
    skill_dir.joinpath("SKILL.md").write_text(
        """---
name: "BrowserOperator"
description: "Operate browser tasks."
tags: ["browser", "research"]
owner_agents: ["researcher"]
recommended_workflows: ["AskGeminiAndReturnAnswer"]
---
# Browser Operator

Extract browser results carefully.
""",
        encoding="utf-8",
    )

    registry = SkillRegistry(tmp_path)
    agent = AgentDefinition(
        name="Researcher",
        description="",
        tags=["research"],
        system_prompt="",
    )

    skills = registry.find_for_agent(agent)
    assert len(skills) == 1
    assert skills[0].name == "BrowserOperator"
    catalog = registry.render_catalog(agent)
    assert "[Skill Library]" in catalog
    assert "AskGeminiAndReturnAnswer" in catalog


def test_review_queue_submits_and_approves_workflow(tmp_path: Path) -> None:
    """Workflow proposals should stay pending until explicitly approved."""
    queue = ReviewQueue(tmp_path)
    workflow = WorkflowDefinition(
        name="DraftFlow",
        description="Temporary workflow",
        owner_agents=["general"],
        tools=["chrome_browser"],
        steps=["open page", "read result"],
        created_by="agent",
        review_status="pending",
    )

    item = queue.submit_workflow(workflow, proposed_by="general")
    assert item.status == "pending"
    assert queue.get(item.review_id) is not None

    approved = queue.approve_workflow(item.review_id)
    assert approved.status == "approved"
    published = tmp_path / "workflows" / "draftflow.yaml"
    assert published.exists()

    registry = WorkflowRegistry(tmp_path)
    assert registry.find("DraftFlow") is not None
