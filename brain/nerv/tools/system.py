"""System tools: the clock, shell access, and delegation to another agent."""

import asyncio
import datetime as dt
import logging
import subprocess
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from nerv.config import get_shell_env_passthrough
from nerv.security.env import scrubbed_env
from nerv.tools._helpers import (
    TIMEZONE_ALIASES,
    _get_project_root,
    _truncate,
)
from nerv.tools.registry import ToolResult, registry

logger = logging.getLogger(__name__)

SHELL_TIMEOUT_SECONDS = 120


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
async def shell(command: str) -> ToolResult:
    """Run a shell command off the event loop, with a timeout and no stdin."""
    try:
        # Children must not inherit provider keys the brain loaded from
        # personal/api_keys.env; only names the user allow-listed pass through.
        env = scrubbed_env(passthrough=get_shell_env_passthrough(_get_project_root()))
        result = await asyncio.to_thread(
            subprocess.run,
            command,
            shell=True,
            env=env,
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
            timeout=SHELL_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired:
        return ToolResult(
            content=f"Shell command timed out after {SHELL_TIMEOUT_SECONDS}s.",
            is_error=True,
        )
    except Exception as e:
        return ToolResult(content=f"Shell execution failed: {e}", is_error=True)

    output = result.stdout
    if result.stderr:
        output += f"\n[stderr]:\n{result.stderr}"

    return ToolResult(
        content=f"Command executed. Exit code: {result.returncode}\nOutput:\n{_truncate(output, tool_name='shell')}",
        is_error=result.returncode != 0,
    )


# -----------------------------------------------------------------------------
# Swarm Tools (Orchestration context needed)
# -----------------------------------------------------------------------------


