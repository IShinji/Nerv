"""Minimal MCP stdio client implementation."""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)

DEFAULT_MCP_PROTOCOL_VERSION = "2024-11-05"


class McpError(RuntimeError):
    """Base error for MCP failures."""


class McpTransportError(McpError):
    """Transport or process-level failure."""


class McpProtocolError(McpError):
    """Protocol or server response failure."""


@dataclass(frozen=True)
class McpServerConfig:
    """Runtime configuration for an MCP server process."""

    name: str
    preset: str = ""
    enabled: bool = False
    transport: str = "stdio"
    command: str = ""
    args: list[str] = field(default_factory=list)
    env: dict[str, str] = field(default_factory=dict)
    cwd: str = ""
    capabilities: list[str] = field(default_factory=list)
    action_map: dict[str, str] = field(default_factory=dict)


class StdioMcpClient:
    """A small JSON-RPC client for MCP stdio servers."""

    def __init__(
        self,
        config: McpServerConfig,
        project_root: Path,
        protocol_version: str = DEFAULT_MCP_PROTOCOL_VERSION,
    ) -> None:
        self.config = config
        self.project_root = project_root
        self.protocol_version = protocol_version
        self._process: asyncio.subprocess.Process | None = None
        self._request_id = 0
        self._pending: dict[int, asyncio.Future[dict[str, Any]]] = {}
        self._send_lock = asyncio.Lock()
        self._stdout_task: asyncio.Task[None] | None = None
        self._stderr_task: asyncio.Task[None] | None = None
        self._initialized = False

    async def connect(self) -> None:
        """Start the server process and complete MCP initialization."""
        if self._initialized:
            return

        if self.config.transport != "stdio":
            raise McpTransportError(
                f"Unsupported MCP transport for {self.config.name}: {self.config.transport}"
            )

        if not self.config.command.strip():
            raise McpTransportError(
                f"MCP server {self.config.name} is enabled but has no command configured."
            )

        env = os.environ.copy()
        env.update(self.config.env)

        cwd = self.project_root
        if self.config.cwd.strip():
            candidate = Path(self.config.cwd).expanduser()
            cwd = candidate if candidate.is_absolute() else (self.project_root / candidate)

        try:
            self._process = await asyncio.create_subprocess_exec(
                self.config.command,
                *self.config.args,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                cwd=str(cwd),
                env=env,
            )
        except FileNotFoundError as exc:
            raise McpTransportError(
                f"MCP server command not found for {self.config.name}: {self.config.command}"
            ) from exc
        except OSError as exc:
            raise McpTransportError(
                f"Failed to start MCP server {self.config.name}: {exc}"
            ) from exc

        self._stdout_task = asyncio.create_task(self._read_stdout_loop())
        self._stderr_task = asyncio.create_task(self._read_stderr_loop())

        initialize_result = await self._send_request(
            "initialize",
            {
                "protocolVersion": self.protocol_version,
                "capabilities": {},
                "clientInfo": {"name": "nerv", "version": "0.1.0"},
            },
        )
        logger.debug(
            "Initialized MCP server %s with protocol %s",
            self.config.name,
            initialize_result.get("protocolVersion", self.protocol_version),
        )
        await self._send_notification("notifications/initialized", {})
        self._initialized = True

    async def list_tools(self) -> list[dict[str, Any]]:
        """Fetch the currently available tool definitions from the server."""
        await self.connect()
        result = await self._send_request("tools/list", {})
        tools = result.get("tools", [])
        if not isinstance(tools, list):
            raise McpProtocolError(f"MCP server {self.config.name} returned invalid tools/list result.")
        return tools

    async def call_tool(self, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        """Invoke a tool on the connected MCP server."""
        await self.connect()
        return await self._send_request(
            "tools/call",
            {"name": name, "arguments": arguments},
        )

    async def close(self) -> None:
        """Terminate the child process and stop background tasks."""
        for task in (self._stdout_task, self._stderr_task):
            if task is not None:
                task.cancel()
                with contextlib.suppress(asyncio.CancelledError):
                    await task

        if self._process is not None:
            if self._process.stdin is not None:
                self._process.stdin.close()
                with contextlib.suppress(Exception):
                    await self._process.stdin.wait_closed()
            if self._process.returncode is None:
                self._process.terminate()
                with contextlib.suppress(ProcessLookupError, asyncio.TimeoutError):
                    await asyncio.wait_for(self._process.wait(), timeout=1.0)
            self._process = None

        for future in self._pending.values():
            if not future.done():
                future.cancel()
        self._pending.clear()
        self._initialized = False

    async def _send_request(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        """Send a JSON-RPC request and await its result."""
        if self._process is None or self._process.stdin is None:
            raise McpTransportError(f"MCP server {self.config.name} is not running.")

        async with self._send_lock:
            self._request_id += 1
            request_id = self._request_id
            future: asyncio.Future[dict[str, Any]] = asyncio.get_running_loop().create_future()
            self._pending[request_id] = future
            payload = {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
                "params": params,
            }

            try:
                self._process.stdin.write(
                    (json.dumps(payload, ensure_ascii=False) + "\n").encode("utf-8")
                )
                await self._process.stdin.drain()
            except Exception as exc:
                self._pending.pop(request_id, None)
                raise McpTransportError(
                    f"Failed to write request to MCP server {self.config.name}: {exc}"
                ) from exc

        try:
            return await asyncio.wait_for(future, timeout=20.0)
        except asyncio.TimeoutError as exc:
            self._pending.pop(request_id, None)
            raise McpTransportError(
                f"MCP server {self.config.name} timed out handling {method}."
            ) from exc

    async def _send_notification(self, method: str, params: dict[str, Any]) -> None:
        """Send a JSON-RPC notification without waiting for a response."""
        if self._process is None or self._process.stdin is None:
            raise McpTransportError(f"MCP server {self.config.name} is not running.")

        payload = {"jsonrpc": "2.0", "method": method, "params": params}
        self._process.stdin.write((json.dumps(payload, ensure_ascii=False) + "\n").encode("utf-8"))
        await self._process.stdin.drain()

    async def _read_stdout_loop(self) -> None:
        """Read JSON-RPC messages from stdout and dispatch responses."""
        assert self._process is not None and self._process.stdout is not None
        stream = self._process.stdout

        while True:
            raw_line = await stream.readline()
            if not raw_line:
                break

            line = raw_line.decode("utf-8").strip()
            if not line:
                continue

            try:
                message = json.loads(line)
            except json.JSONDecodeError as exc:
                logger.warning("Invalid MCP JSON from %s: %s", self.config.name, exc)
                continue

            if "id" not in message:
                logger.debug("Ignoring MCP notification from %s: %s", self.config.name, message)
                continue

            future = self._pending.pop(message["id"], None)
            if future is None or future.done():
                continue

            if "error" in message:
                error = message["error"]
                future.set_exception(
                    McpProtocolError(
                        f"MCP server {self.config.name} error: {error.get('message', error)}"
                    )
                )
            else:
                future.set_result(message.get("result", {}))

    async def _read_stderr_loop(self) -> None:
        """Drain stderr so child process logging does not block the pipe."""
        assert self._process is not None and self._process.stderr is not None
        stream = self._process.stderr

        while True:
            raw_line = await stream.readline()
            if not raw_line:
                break
            logger.debug(
                "[mcp:%s stderr] %s",
                self.config.name,
                raw_line.decode("utf-8", errors="replace").rstrip(),
            )
