"""System tools: the clock, shell access, and delegation to another agent."""

import datetime as dt
import logging
import pathlib
import subprocess
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from nerv.tools._helpers import (
    TIMEZONE_ALIASES,
    _truncate,
)
from nerv.tools.registry import ToolResult, registry

logger = logging.getLogger(__name__)


@registry.register(
    name="current_time",
    description=(
        "Get the current date and time. Use an IANA timezone like "
        "America/Los_Angeles or a common city like Seattle. "
        "Use this instead of guessing current time/date."
    ),
    requires_confirmation=False,
)
def current_time(timezone: str = "local") -> ToolResult:
    """Return the current date and time for the requested timezone."""
    normalized = timezone.strip()

    try:
        if not normalized or normalized.lower() == "local":
            current = dt.datetime.now().astimezone()
            timezone_label = str(current.tzinfo)
        else:
            zone_name = TIMEZONE_ALIASES.get(normalized.lower(), normalized)
            current = dt.datetime.now(ZoneInfo(zone_name))
            timezone_label = zone_name
    except ZoneInfoNotFoundError:
        return ToolResult(
            content=(
                f"Unknown timezone '{timezone}'. Please use an IANA timezone like "
                "America/Los_Angeles."
            ),
            is_error=True,
        )

    return ToolResult(
        content=(
            f"Current time in {timezone_label}: "
            f"{current.strftime('%Y-%m-%d %H:%M:%S %Z')}"
        )
    )


@registry.register(
    name="shell",
    description="Execute a shell command on the host OS. This includes running build scripts, python commands, git, etc.",
    requires_confirmation=True,  # Shell is always dangerous
)
def shell(command: str) -> ToolResult:
    """Run a shell command."""
    try:
        result = subprocess.run(
            command,
            shell=True,
            capture_output=True,
            text=True,
        )
    except Exception as e:
        return ToolResult(content=f"Shell execution failed: {e}", is_error=True)

    output = result.stdout
    if result.stderr:
        output += f"\n[stderr]:\n{result.stderr}"

    return ToolResult(
        content=f"Command executed. Exit code: {result.returncode}\nOutput:\n{_truncate(output)}",
        is_error=result.returncode != 0,
    )


# -----------------------------------------------------------------------------
# Swarm Tools (Orchestration context needed)
# -----------------------------------------------------------------------------


@registry.register(
    name="delegate_task",
    description="Ask another specialized professional agent to do a task and return the result to you. Use this to consult domain experts.",
    requires_confirmation=False,
)
async def delegate_task(role_name: str, task_description: str) -> ToolResult:
    """Spawn or consult a sub-agent for a specific task."""

    from nerv.orchestrator.orchestrator import Orchestrator

    # We build a temporary orchestrator for the sub-agent
    orch = Orchestrator(pathlib.Path.cwd())

    # Fake the route result
    from nerv.models import RouteResult

    route = RouteResult(
        intent=role_name,
        complexity="high",
        model_tier=1,
        agent_type=role_name,  # This forces the Factory to spawn this exact role!
        reply="",
    )

    # We prefix the message so the sub-agent knows its context
    prompt = f"[DELEGATION TASK]\nYou have been spawned by a higher-level architect agent to handle a sub-task.\nTask Description: {task_description}"

    response = await orch.dispatch(prompt, route)

    return ToolResult(content=f"Expert ({role_name}) replied: {response}")
