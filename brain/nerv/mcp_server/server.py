"""Stdio MCP server exposing Nerv native tools to the Claude Code CLI.

Each registered Nerv tool becomes an MCP tool. Before executing, the server
applies the workspace sandbox policy: safe / in-sandbox calls run immediately,
while high-risk or out-of-sandbox calls are enqueued in the shared pending
store and reported back so the user can approve them inside Nerv.
"""

from __future__ import annotations

import asyncio
import inspect
import logging
import os
from pathlib import Path
from typing import Any

import mcp.types as types
from mcp.server import Server
from mcp.server.stdio import stdio_server

from nerv.config import get_workspace_root
from nerv.memory.manager import DEFAULT_SCOPE, use_scope
from nerv.security import PendingActionStore, confirmation_reason
from nerv.tools import builtins as _builtins  # noqa: F401  (registers core tools)
from nerv.tools import (
    evolution_tools as _evolution_tools,  # noqa: F401  (agent evolution)
)
from nerv.tools import (
    productivity as _productivity,  # noqa: F401  (calendar/notes/email)
)
from nerv.tools.builtins import registry

logger = logging.getLogger(__name__)

SERVER_NAME = "nerv"
# Env var through which the launcher hands the conversation scope to this
# process, so memory tools read the right sender's history (fails closed to
# the default scope when unset).
SCOPE_ENV = "NERV_MEMORY_SCOPE"


def _project_root() -> Path:
    """Resolve the Nerv project root (passed by the launcher via env)."""
    env_root = os.environ.get("NERV_PROJECT_ROOT")
    if env_root:
        return Path(env_root).expanduser().resolve()
    return Path.cwd().resolve()


async def execute_or_suspend(
    name: str,
    arguments: dict[str, Any],
    workspace_root: Path,
    store: PendingActionStore,
) -> str:
    """Run a Nerv tool, or suspend it for confirmation per the sandbox policy.

    Returns the text the model should see. Out-of-sandbox / high-risk calls are
    enqueued in the shared store and reported as suspended rather than executed.
    """
    arguments = arguments or {}
    tool_def = registry.get_tool(name)
    if tool_def is None:
        return f"Unknown tool: {name}"

    reason = confirmation_reason(
        name,
        arguments,
        workspace_root,
        requires_confirmation=tool_def.requires_confirmation,
        project_root=_project_root(),
    )
    if reason:
        record = store.add(
            name, arguments, reason=reason, scope=os.environ.get(SCOPE_ENV, "")
        )
        logger.info("Suspended %s (#%d): %s", name, record.id, reason)
        return (
            f"[ACTION SUSPENDED #{record.id}] {reason}. This action was NOT "
            f"executed. The user must approve it in Nerv with 'confirm {record.id}' "
            "before it runs. Do not retry — tell the user what is pending and "
            "continue with anything else you can do safely."
        )

    try:
        if inspect.iscoroutinefunction(tool_def.func):
            result = await tool_def.func(**arguments)
        else:
            result = await asyncio.to_thread(tool_def.func, **arguments)
    except Exception as exc:  # surface tool errors to the model, don't crash
        logger.exception("Tool %s raised", name)
        return f"Tool {name} failed: {exc}"

    return result.content


def build_server(project_root: Path | None = None) -> Server:
    """Construct the MCP server bound to a project root."""
    root = (project_root or _project_root()).resolve()
    workspace_root = get_workspace_root(root)
    store = PendingActionStore(root)
    server: Server = Server(SERVER_NAME)

    @server.list_tools()
    async def list_tools() -> list[types.Tool]:
        tools: list[types.Tool] = []
        for name, tool_def in registry._tools.items():
            parameters = tool_def.schema.get("function", {}).get(
                "parameters", {"type": "object", "properties": {}}
            )
            tools.append(
                types.Tool(
                    name=name,
                    description=tool_def.description,
                    inputSchema=parameters,
                )
            )
        return tools

    @server.call_tool()
    async def call_tool(
        name: str, arguments: dict[str, Any]
    ) -> list[types.TextContent]:
        with use_scope(os.environ.get(SCOPE_ENV, DEFAULT_SCOPE)):
            text = await execute_or_suspend(name, arguments, workspace_root, store)
        return [types.TextContent(type="text", text=text)]

    return server


async def run() -> None:
    """Run the Nerv MCP server over stdio."""
    server = build_server()
    async with stdio_server() as (read_stream, write_stream):
        await server.run(
            read_stream,
            write_stream,
            server.create_initialization_options(),
        )
