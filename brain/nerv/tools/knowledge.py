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
    name="execute_workflow",
    description=(
        "Execute a named shared workflow. This delegates control to the Workflow Executor engine "
        "which will sequentially perform the steps and return the final structurally extracted response."
    ),
    requires_confirmation=False,
)
async def execute_workflow(workflow_name: str, inputs: str) -> ToolResult:
    """Execute a shared workflow via the native WorkflowExecutor."""
    # We must fetch the orchestrator from ipc to inject it into the executor
    # To avoid circular import, we fetch it locally
    from nerv.ipc import _get_orchestrator
    from nerv.workflows.executor import WorkflowExecutor
    from nerv.workflows.registry import WorkflowRegistry

    registry_obj = WorkflowRegistry(_get_project_root())
    workflow = registry_obj.find(workflow_name.strip())

    if not workflow:
        return ToolResult(
            content=f"Error: Shared workflow '{workflow_name}' not found. List workflows first.",
            is_error=True,
        )

    if not workflow.steps:
        return ToolResult(
            content=f"Error: Workflow '{workflow.name}' has no defined steps.",
            is_error=True,
        )

    orchestrator = _get_orchestrator()
    executor = WorkflowExecutor(orchestrator)

    try:
        final_result = await executor.execute(workflow, inputs)
        return ToolResult(
            content=f"Workflow executed successfully.\\nResult context:\\n{final_result}"
        )
    except Exception as e:
        return ToolResult(content=f"Workflow execution failed: {e}", is_error=True)


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


@registry.register(
    name="mcp_presets",
    description=(
        "List builtin MCP presets or render a config snippet for one preset. "
        "Use this to configure a known MCP server such as the official Chrome "
        "DevTools MCP integration."
    ),
    requires_confirmation=False,
)
def mcp_presets(preset_name: str = "") -> ToolResult:
    """List builtin MCP presets or render one preset snippet."""
    from nerv.mcp.presets import (
        get_builtin_mcp_preset,
        list_builtin_mcp_presets,
        render_mcp_preset_snippet,
    )

    normalized = preset_name.strip()
    if not normalized:
        presets = list_builtin_mcp_presets()
        if not presets:
            return ToolResult(content="No builtin MCP presets are available.")

        lines = ["Builtin MCP presets:"]
        for preset in presets:
            capabilities = (
                ", ".join(preset.capabilities) if preset.capabilities else "none"
            )
            lines.append(f"- {preset.name}: {preset.description}")
            lines.append(f"  Capabilities: {capabilities}")
            lines.append(f"  Source: {preset.source_url}")
        return ToolResult(content="\n".join(lines))

    preset = get_builtin_mcp_preset(normalized)
    if preset is None:
        return ToolResult(content=f"Unknown MCP preset: {preset_name}", is_error=True)

    lines = [
        f"Preset: {preset.name}",
        preset.description,
        f"Source: {preset.source_url}",
    ]
    if preset.default_action_map:
        lines.append(
            "Mapped actions: "
            + ", ".join(
                f"{action}->{tool_name}"
                for action, tool_name in sorted(preset.default_action_map.items())
            )
        )
    if preset.notes:
        lines.append("Notes:")
        lines.extend(f"- {note}" for note in preset.notes)
    lines.append("Config snippet:")
    lines.append(render_mcp_preset_snippet(preset))
    return ToolResult(content="\n".join(lines))


@registry.register(
    name="mcp_status",
    description=(
        "Inspect configured MCP servers, their health, bound capabilities, and "
        "available remote tools. Optionally pass a specific server name."
    ),
    requires_confirmation=False,
)
async def mcp_status(server_name: str = "") -> ToolResult:
    """Report MCP server configuration and health."""
    from nerv.mcp import get_mcp_manager
    from nerv.mcp.client import McpTransportError

    manager = await get_mcp_manager(_get_project_root())
    try:
        statuses = await manager.get_status(server_name)
    except McpTransportError as exc:
        return ToolResult(content=str(exc), is_error=True)

    if not statuses:
        return ToolResult(content="No MCP servers are configured.")

    lines = ["MCP server status:"]
    for status in statuses:
        health = "healthy" if status["healthy"] else "unhealthy"
        if not status["enabled"]:
            health = "disabled"

        lines.append(f"- {status['name']}: {health}")
        lines.append(f"  Transport: {status['transport']}")
        command = status["command"] or "(unset)"
        args = " ".join(status["args"]) if status["args"] else "(none)"
        lines.append(f"  Command: {command}")
        lines.append(f"  Args: {args}")
        if status["preset"]:
            lines.append(f"  Preset: {status['preset']}")
        capabilities = (
            ", ".join(status["capabilities"]) if status["capabilities"] else "none"
        )
        lines.append(f"  Capabilities: {capabilities}")
        if status["action_map"]:
            lines.append(
                "  Action map: "
                + ", ".join(
                    f"{action}->{tool_name}"
                    for action, tool_name in sorted(status["action_map"].items())
                )
            )
        if status["available_tools"]:
            lines.append("  Remote tools: " + ", ".join(status["available_tools"]))
        if status["error"]:
            lines.append(f"  Error: {status['error']}")

    return ToolResult(content="\n".join(lines))
