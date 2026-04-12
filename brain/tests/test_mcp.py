"""Tests for MCP config loading and browser MCP fallback behavior."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

import nerv.tools.builtins as builtins
from nerv.config import load_project_config
from nerv.mcp import reset_mcp_managers
from nerv.mcp.manager import McpServerManager
from nerv.tools.registry import ToolResult


def _write_project_config(tmp_path: Path, user_config: str) -> None:
    (tmp_path / "core").mkdir(parents=True, exist_ok=True)
    (tmp_path / "core" / "config.default.yaml").write_text(
        """
models:
  router:
    provider: "ollama"
    model: "qwen2.5:1.5b"
    base_url: "http://localhost:11434"
""".strip(),
        encoding="utf-8",
    )
    (tmp_path / "config.yaml").write_text(user_config.strip(), encoding="utf-8")


def test_load_project_config_merges_mcp_servers(tmp_path: Path) -> None:
    """Python config loader should expose merged MCP config."""
    _write_project_config(
        tmp_path,
        """
mcp:
  protocol_version: "2024-11-05"
  servers:
    chrome:
      enabled: true
      transport: "stdio"
      command: "python"
      args: ["server.py"]
      capabilities: ["browser.interactive"]
      action_map:
        open_url: "open_url"
""",
    )

    config = load_project_config(tmp_path)
    assert config["models"]["router"]["model"] == "qwen2.5:1.5b"
    assert config["mcp"]["servers"]["chrome"]["enabled"] is True
    assert config["mcp"]["servers"]["chrome"]["capabilities"] == ["browser.interactive"]


def test_mcp_manager_detects_capability_binding(tmp_path: Path) -> None:
    """Manager should detect when a capability has an enabled MCP binding."""
    _write_project_config(
        tmp_path,
        """
mcp:
  servers:
    chrome:
      enabled: true
      transport: "stdio"
      command: "python"
      args: ["server.py"]
      capabilities: ["browser.interactive"]
      action_map:
        open_url: "open_url"
""",
    )

    manager = McpServerManager(tmp_path)
    assert manager.has_capability_binding("browser.interactive") is True
    assert manager.has_capability_binding("browser.read") is False


@pytest.mark.asyncio
async def test_mcp_manager_status_reports_disabled_server(tmp_path: Path) -> None:
    """Status output should include disabled server config without launching it."""
    _write_project_config(
        tmp_path,
        """
mcp:
  servers:
    chrome:
      enabled: false
      transport: "stdio"
      command: "python"
      args: ["server.py"]
      capabilities: ["browser.interactive"]
""",
    )

    manager = McpServerManager(tmp_path)
    statuses = await manager.get_status()
    assert len(statuses) == 1
    assert statuses[0]["name"] == "chrome"
    assert statuses[0]["enabled"] is False
    assert statuses[0]["healthy"] is False
    assert "disabled" in statuses[0]["error"].lower()


@pytest.mark.asyncio
async def test_browser_interactive_prefers_mcp_server(tmp_path: Path, monkeypatch) -> None:
    """browser_interactive should route through MCP when configured and available."""
    server_script = tmp_path / "fake_mcp_server.py"
    server_script.write_text(
        """
import json
import sys

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    if method == "initialize":
        response = {
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {
                "protocolVersion": message["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake-chrome", "version": "0.1.0"},
            },
        }
        sys.stdout.write(json.dumps(response) + "\\n")
        sys.stdout.flush()
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        response = {
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {
                "tools": [
                    {
                        "name": "open_url",
                        "description": "Open a URL",
                        "inputSchema": {
                            "type": "object",
                            "properties": {"url": {"type": "string"}},
                            "required": ["url"],
                        },
                    }
                ]
            },
        }
        sys.stdout.write(json.dumps(response) + "\\n")
        sys.stdout.flush()
    elif method == "tools/call":
        response = {
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {
                "content": [
                    {"type": "text", "text": "MCP opened " + message["params"]["arguments"]["url"]}
                ]
            },
        }
        sys.stdout.write(json.dumps(response) + "\\n")
        sys.stdout.flush()
""".strip(),
        encoding="utf-8",
    )

    _write_project_config(
        tmp_path,
        f"""
mcp:
  servers:
    chrome:
      enabled: true
      transport: "stdio"
      command: "{sys.executable}"
      args: ["{server_script}"]
      capabilities: ["browser.interactive"]
      action_map:
        open_url: "open_url"
""",
    )

    monkeypatch.setattr(builtins, "_get_project_root", lambda: tmp_path)
    monkeypatch.setattr(
        builtins,
        "chrome_browser",
        lambda **kwargs: ToolResult(content="native fallback should not run", is_error=True),
    )

    await reset_mcp_managers()
    try:
        result = await builtins.browser_interactive(
            action="open_url",
            url="https://example.com",
        )
    finally:
        await reset_mcp_managers()

    assert result.is_error is False
    assert "MCP opened https://example.com" in result.content


@pytest.mark.asyncio
async def test_mcp_status_tool_reports_remote_tools(tmp_path: Path, monkeypatch) -> None:
    """The mcp_status tool should surface remote tools for a healthy server."""
    server_script = tmp_path / "fake_mcp_status_server.py"
    server_script.write_text(
        """
import json
import sys

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    if method == "initialize":
        response = {
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {
                "protocolVersion": message["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake-chrome", "version": "0.1.0"},
            },
        }
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        response = {
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {
                "tools": [
                    {"name": "open_url", "description": "Open URL", "inputSchema": {"type": "object"}},
                    {"name": "fill_prompt", "description": "Fill prompt", "inputSchema": {"type": "object"}},
                ]
            },
        }
    else:
        response = {"jsonrpc": "2.0", "id": message["id"], "result": {"content": []}}
    sys.stdout.write(json.dumps(response) + "\\n")
    sys.stdout.flush()
""".strip(),
        encoding="utf-8",
    )

    _write_project_config(
        tmp_path,
        f"""
mcp:
  servers:
    chrome:
      enabled: true
      transport: "stdio"
      command: "{sys.executable}"
      args: ["{server_script}"]
      capabilities: ["browser.interactive"]
      action_map:
        open_url: "open_url"
""",
    )

    monkeypatch.setattr(builtins, "_get_project_root", lambda: tmp_path)

    await reset_mcp_managers()
    try:
        result = await builtins.mcp_status()
    finally:
        await reset_mcp_managers()

    assert result.is_error is False
    assert "chrome: healthy" in result.content
    assert "Remote tools: fill_prompt, open_url" in result.content


@pytest.mark.asyncio
async def test_browser_interactive_falls_back_to_native(tmp_path: Path, monkeypatch) -> None:
    """browser_interactive should use the native fallback when MCP is unavailable."""
    _write_project_config(
        tmp_path,
        """
mcp:
  servers:
    chrome:
      enabled: true
      transport: "stdio"
      command: "definitely-missing-command"
      args: []
      capabilities: ["browser.interactive"]
      action_map:
        open_url: "open_url"
""",
    )

    monkeypatch.setattr(builtins, "_get_project_root", lambda: tmp_path)
    monkeypatch.setattr(
        builtins,
        "chrome_browser",
        lambda **kwargs: ToolResult(content=f"native fallback {kwargs['url']}"),
    )

    await reset_mcp_managers()
    try:
        result = await builtins.browser_interactive(
            action="open_url",
            url="https://fallback.example",
        )
    finally:
        await reset_mcp_managers()

    assert result.is_error is False
    assert "native fallback https://fallback.example" in result.content
