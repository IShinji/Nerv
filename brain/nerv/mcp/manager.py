"""Config-driven MCP provider manager."""

from __future__ import annotations

import asyncio
import json
import logging
from pathlib import Path
from typing import Any

from nerv.config import load_project_config
from nerv.mcp.client import (
    DEFAULT_MCP_PROTOCOL_VERSION,
    McpProtocolError,
    McpServerConfig,
    McpTransportError,
    StdioMcpClient,
)
from nerv.mcp.presets import (
    get_builtin_mcp_preset,
    merge_preset_into_server_config,
)
from nerv.tools.registry import ToolResult

logger = logging.getLogger(__name__)


class McpServerManager:
    """Own configured MCP servers and capability bindings."""

    def __init__(self, project_root: Path) -> None:
        self.project_root = project_root
        self.protocol_version = DEFAULT_MCP_PROTOCOL_VERSION
        self._server_configs: dict[str, McpServerConfig] = {}
        self._clients: dict[str, StdioMcpClient] = {}
        self._tool_cache: dict[str, set[str]] = {}
        self._load_config()

    def has_capability_binding(self, capability_name: str) -> bool:
        """Return whether any enabled MCP server is bound to the capability."""
        return self._find_server_for_capability(capability_name) is not None

    async def get_status(self, server_name: str = "") -> list[dict[str, Any]]:
        """Collect status information for one or all configured MCP servers."""
        normalized = server_name.strip()
        if normalized:
            config = self._server_configs.get(normalized)
            if config is None:
                raise McpTransportError(f"Unknown MCP server: {server_name}")
            return [await self._build_status_entry(config)]

        return [
            await self._build_status_entry(config)
            for config in self._server_configs.values()
        ]

    async def invoke_capability_action(
        self,
        capability_name: str,
        action: str,
        arguments: dict[str, Any],
    ) -> ToolResult | None:
        """Invoke an MCP-bound tool for a logical capability action."""
        config = self._find_server_for_capability(capability_name)
        if config is None:
            return None

        tool_name = config.action_map.get(action)
        if not tool_name:
            logger.debug(
                "No MCP action map for capability %s action %s on server %s",
                capability_name,
                action,
                config.name,
            )
            return None

        preset = get_builtin_mcp_preset(config.preset) if config.preset else None
        binding = preset.action_bindings.get(action) if preset is not None else None
        request_arguments = arguments
        if binding is not None and tool_name == binding.tool_name:
            request_arguments = binding.build_arguments(arguments)

        client = await self._get_client(config)
        tool_names = await self._get_tool_names(config.name, client)
        if tool_name not in tool_names:
            logger.warning(
                "Configured MCP tool %s is unavailable on server %s; using fallback.",
                tool_name,
                config.name,
            )
            return None

        result = await client.call_tool(tool_name, request_arguments)
        return self._format_tool_result(config.name, tool_name, result)

    async def close_all(self) -> None:
        """Close all active MCP clients."""
        for client in list(self._clients.values()):
            await client.close()
        self._clients.clear()
        self._tool_cache.clear()

    async def _build_status_entry(self, config: McpServerConfig) -> dict[str, Any]:
        """Build a structured status snapshot for one configured server."""
        entry: dict[str, Any] = {
            "name": config.name,
            "preset": config.preset,
            "enabled": config.enabled,
            "transport": config.transport,
            "command": config.command,
            "args": list(config.args),
            "cwd": config.cwd,
            "capabilities": list(config.capabilities),
            "action_map": dict(config.action_map),
            "healthy": False,
            "available_tools": [],
            "error": "",
        }

        if not config.enabled:
            entry["error"] = "Server is disabled."
            return entry

        if not config.command:
            entry["error"] = "Server is enabled but command is empty."
            return entry

        try:
            client = await self._get_client(config)
            tool_names = sorted(await self._get_tool_names(config.name, client))
            entry["healthy"] = True
            entry["available_tools"] = tool_names
            return entry
        except (McpTransportError, McpProtocolError) as exc:
            entry["error"] = str(exc)
            return entry

    def _load_config(self) -> None:
        """Load and normalize MCP config from merged project config."""
        config = load_project_config(self.project_root)
        mcp_config = config.get("mcp", {}) or {}
        if not isinstance(mcp_config, dict):
            return

        protocol_version = mcp_config.get("protocol_version")
        if isinstance(protocol_version, str) and protocol_version.strip():
            self.protocol_version = protocol_version.strip()

        raw_servers = mcp_config.get("servers", {}) or {}
        if not isinstance(raw_servers, dict):
            return

        for name, raw_value in raw_servers.items():
            if not isinstance(raw_value, dict):
                continue

            self._server_configs[name] = merge_preset_into_server_config(
                name,
                raw_value,
            )

    def _find_server_for_capability(
        self, capability_name: str
    ) -> McpServerConfig | None:
        """Find the first enabled MCP server bound to the capability."""
        for config in self._server_configs.values():
            if not config.enabled:
                continue
            if capability_name in config.capabilities:
                return config
        return None

    async def _get_client(self, config: McpServerConfig) -> StdioMcpClient:
        """Get or lazily start the client for a configured server."""
        client = self._clients.get(config.name)
        if client is None:
            client = StdioMcpClient(
                config,
                project_root=self.project_root,
                protocol_version=self.protocol_version,
            )
            self._clients[config.name] = client
        await client.connect()
        return client

    async def _get_tool_names(
        self, server_name: str, client: StdioMcpClient
    ) -> set[str]:
        """Fetch and cache tool names for a server."""
        cached = self._tool_cache.get(server_name)
        if cached is not None:
            return cached

        tools = await client.list_tools()
        tool_names = {
            str(tool.get("name", "")).strip() for tool in tools if tool.get("name")
        }
        self._tool_cache[server_name] = tool_names
        return tool_names

    def _format_tool_result(
        self,
        server_name: str,
        tool_name: str,
        result: dict[str, Any],
    ) -> ToolResult:
        """Convert MCP CallToolResult payloads into Nerv tool results."""
        is_error = bool(result.get("isError", False))
        lines: list[str] = []

        structured = result.get("structuredContent")
        if structured not in (None, {}):
            lines.append(json.dumps(structured, ensure_ascii=False))

        for item in result.get("content", []) or []:
            if not isinstance(item, dict):
                lines.append(str(item))
                continue
            if item.get("type") == "text":
                lines.append(str(item.get("text", "")))
            else:
                lines.append(json.dumps(item, ensure_ascii=False))

        if not lines:
            lines.append(f"MCP tool {tool_name} on {server_name} returned no content.")

        return ToolResult(
            content="\n".join(line for line in lines if line), is_error=is_error
        )


_mcp_managers: dict[Path, McpServerManager] = {}
_mcp_manager_lock = asyncio.Lock()


async def get_mcp_manager(project_root: Path) -> McpServerManager:
    """Return a cached MCP manager for the project root."""
    normalized = project_root.resolve()
    async with _mcp_manager_lock:
        manager = _mcp_managers.get(normalized)
        if manager is None:
            manager = McpServerManager(normalized)
            _mcp_managers[normalized] = manager
        return manager


async def reset_mcp_managers() -> None:
    """Close and clear all cached MCP managers. Intended for tests."""
    async with _mcp_manager_lock:
        managers = list(_mcp_managers.values())
        _mcp_managers.clear()

    for manager in managers:
        try:
            await manager.close_all()
        except (McpTransportError, McpProtocolError):
            logger.debug("Ignoring MCP manager cleanup failure.", exc_info=True)
