"""Knowledge tools: the workflow library, skills, review queue, and MCP status."""

import logging

from nerv.tools._helpers import (
    _get_project_root,
    _parse_csv_or_lines,
    _truncate,
)
from nerv.tools.registry import ToolResult, registry

logger = logging.getLogger(__name__)


@registry.register(
    name="list_workflows",
    description=(
        "List shared workflows relevant to an agent or task domain. "
        "Use this before drafting a new workflow."
    ),
    requires_confirmation=False,
)
def list_workflows(agent_name: str = "") -> ToolResult:
    """List approved shared workflows."""
    from nerv.workflows.registry import WorkflowRegistry

    project_root = _get_project_root()
    workflow_registry = WorkflowRegistry(project_root)
    workflows = workflow_registry.list_workflows()
    if agent_name.strip():
        filtered = []
        target = agent_name.strip().lower()
        for workflow in workflows:
            owners = {owner.lower() for owner in workflow.owner_agents}
            if target in owners:
                filtered.append(workflow)
        workflows = filtered

    if not workflows:
        return ToolResult(content="No shared workflows found.")

    lines = ["Shared workflows:"]
    for workflow in workflows:
        owners = (
            ", ".join(workflow.owner_agents) if workflow.owner_agents else "all agents"
        )
        lines.append(f"- {workflow.name}: {workflow.description}")
        lines.append(f"  Owners: {owners}")
        if workflow.tools:
            lines.append(f"  Tools: {', '.join(workflow.tools)}")
    return ToolResult(content="\n".join(lines))


@registry.register(
    name="get_workflow",
    description="Read a shared workflow definition by name, including steps and success criteria.",
    requires_confirmation=False,
)
def get_workflow(name: str) -> ToolResult:
    """Get details for a specific shared workflow."""
    from nerv.workflows.registry import WorkflowRegistry

    workflow = WorkflowRegistry(_get_project_root()).find(name)
    if workflow is None:
        return ToolResult(content=f"Workflow not found: {name}", is_error=True)

    lines = [f"Workflow: {workflow.name}", workflow.description]
    if workflow.steps:
        lines.append("Steps:")
        lines.extend(f"- {step}" for step in workflow.steps)
    if workflow.success_criteria:
        lines.append("Success criteria:")
        lines.extend(f"- {item}" for item in workflow.success_criteria)
    return ToolResult(content="\n".join(lines))


@registry.register(
    name="propose_workflow",
    description=(
        "Draft a new workflow proposal and send it to the review queue. "
        "Input list fields as comma-separated or newline-separated text."
    ),
    requires_confirmation=False,
)
def propose_workflow(
    name: str,
    description: str,
    tags: str = "",
    owner_agents: str = "",
    tools: str = "",
    steps: str = "",
    success_criteria: str = "",
) -> ToolResult:
    """Create a pending workflow review item."""
    from nerv.workflows.models import WorkflowDefinition
    from nerv.workflows.registry import ReviewQueue

    workflow = WorkflowDefinition(
        name=name.strip(),
        description=description.strip(),
        tags=_parse_csv_or_lines(tags),
        owner_agents=_parse_csv_or_lines(owner_agents),
        tools=_parse_csv_or_lines(tools),
        steps=_parse_csv_or_lines(steps),
        success_criteria=_parse_csv_or_lines(success_criteria),
        created_by="agent",
        review_status="pending",
    )

    review_item = ReviewQueue(_get_project_root()).submit_workflow(
        workflow,
        proposed_by="agent",
    )
    return ToolResult(
        content=(
            f"Workflow proposal queued for review: {review_item.review_id}\n"
            f"Name: {review_item.workflow.name}\n"
            'Ask the user to review it with "show workflow reviews", '
            f'"approve workflow {review_item.review_id}", or '
            f'"reject workflow {review_item.review_id}".'
        )
    )


@registry.register(
    name="list_skills",
    description="List shared skills relevant to an agent or task domain.",
    requires_confirmation=False,
)
def list_skills(agent_name: str = "") -> ToolResult:
    """List available skills."""
    from nerv.skills.registry import SkillRegistry

    skills = SkillRegistry(_get_project_root()).list_skills()
    if agent_name.strip():
        target = agent_name.strip().lower()
        skills = [
            skill
            for skill in skills
            if target in {owner.lower() for owner in skill.owner_agents}
        ]

    if not skills:
        return ToolResult(content="No skills found.")

    lines = ["Skills:"]
    for skill in skills:
        lines.append(f"- {skill.name}: {skill.description}")
        if skill.recommended_workflows:
            lines.append(
                "  Recommended workflows: " + ", ".join(skill.recommended_workflows)
            )
    return ToolResult(content="\n".join(lines))


@registry.register(
    name="get_skill",
    description="Read the details of a specific skill by name.",
    requires_confirmation=False,
)
def get_skill(name: str) -> ToolResult:
    """Get a skill description and body."""
    from nerv.skills.registry import SkillRegistry

    skill = SkillRegistry(_get_project_root()).find(name)
    if skill is None:
        return ToolResult(content=f"Skill not found: {name}", is_error=True)

    body = _truncate(skill.body.strip(), 3000)
    lines = [f"Skill: {skill.name}", skill.description]
    if skill.recommended_workflows:
        lines.append("Recommended workflows: " + ", ".join(skill.recommended_workflows))
    if body:
        lines.append("Body:")
        lines.append(body)
    return ToolResult(content="\n".join(lines))


@registry.register(
    name="list_review_queue",
    description="List pending or historical workflow review items.",
    requires_confirmation=False,
)
def list_review_queue(status: str = "pending") -> ToolResult:
    """List review items for workflows."""
    from nerv.workflows.registry import ReviewQueue

    items = ReviewQueue(_get_project_root()).list_reviews(status=status.strip())
    if not items:
        return ToolResult(
            content=f"No workflow review items found for status: {status}"
        )

    lines = [f"Workflow review items ({status}):"]
    for item in items:
        lines.append(
            f"- {item.review_id}: {item.workflow.name} [{item.status}] - {item.workflow.description}"
        )
    return ToolResult(content="\n".join(lines))
