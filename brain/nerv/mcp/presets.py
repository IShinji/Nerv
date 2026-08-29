"""Builtin MCP server presets and action bindings."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

import yaml

from nerv.mcp.client import McpServerConfig

ArgumentBuilder = Callable[[dict[str, Any]], dict[str, Any]]


@dataclass(frozen=True)
class McpActionBinding:
    """Map a logical Nerv action to a concrete MCP tool call."""

    tool_name: str
    build_arguments: ArgumentBuilder


@dataclass(frozen=True)
class McpServerPreset:
    """Builtin preset for a known MCP server."""

    name: str
    description: str
    source_url: str
    notes: tuple[str, ...] = ()
    transport: str = "stdio"
    command: str = ""
    args: tuple[str, ...] = ()
    env: dict[str, str] = field(default_factory=dict)
    cwd: str = ""
    capabilities: tuple[str, ...] = ()
    default_action_map: dict[str, str] = field(default_factory=dict)
    action_bindings: dict[str, McpActionBinding] = field(default_factory=dict)


def _passthrough_url(arguments: dict[str, Any]) -> dict[str, Any]:
    """Forward the normalized URL to the MCP tool."""
    return {"url": str(arguments.get("url", ""))}


def _empty_arguments(arguments: dict[str, Any]) -> dict[str, Any]:
    """Ignore Nerv action arguments."""
    del arguments
    return {}


def _evaluate_script_arguments(arguments: dict[str, Any]) -> dict[str, Any]:
    """Wrap raw script text into Chrome DevTools MCP's function API."""
    return {
        "function": "(source) => { return globalThis.eval(source); }",
        "args": [str(arguments.get("script", ""))],
    }


def _submit_prompt_arguments(arguments: dict[str, Any]) -> dict[str, Any]:
    """Submit the active prompt box with Enter."""
    del arguments
    return {"key": "Enter"}


def _wait_for_text_arguments(arguments: dict[str, Any]) -> dict[str, Any]:
    """Convert Nerv polling arguments to Chrome DevTools MCP wait arguments."""
    timeout_secs = max(1, int(arguments.get("timeout_secs", 30)))
    return {
        "text": [str(arguments.get("wait_for", ""))],
        "timeout": timeout_secs * 1000,
    }


BUILTIN_MCP_PRESETS: dict[str, McpServerPreset] = {
    "chrome_devtools_official": McpServerPreset(
        name="chrome_devtools_official",
        description=(
            "Official Chrome DevTools MCP server for interactive browser control."
        ),
        source_url="https://github.com/ChromeDevTools/chrome-devtools-mcp",
        notes=(
            "Uses the official Chrome DevTools MCP package via npx.",
            "Best fit for browser.interactive capability in MCP-first mode.",
            "Supports richer actions than Nerv's current generic browser abstraction.",
        ),
        transport="stdio",
        command="npx",
        args=("-y", "chrome-devtools-mcp@latest"),
        capabilities=("browser.interactive",),
        default_action_map={
            "open_url": "new_page",
            "get_page_text": "take_snapshot",
            "run_javascript": "evaluate_script",
            "submit_prompt": "press_key",
            "wait_for_text": "wait_for",
        },
        action_bindings={
            "open_url": McpActionBinding("new_page", _passthrough_url),
            "get_page_text": McpActionBinding("take_snapshot", _empty_arguments),
            "run_javascript": McpActionBinding(
                "evaluate_script",
                _evaluate_script_arguments,
            ),
            "submit_prompt": McpActionBinding("press_key", _submit_prompt_arguments),
            "wait_for_text": McpActionBinding("wait_for", _wait_for_text_arguments),
        },
    )
}


def get_builtin_mcp_preset(name: str) -> McpServerPreset | None:
    """Look up a builtin MCP preset by name."""
    normalized = name.strip()
    if not normalized:
        return None
    return BUILTIN_MCP_PRESETS.get(normalized)


def list_builtin_mcp_presets() -> list[McpServerPreset]:
    """Return builtin MCP presets in a stable order."""
    return [BUILTIN_MCP_PRESETS[name] for name in sorted(BUILTIN_MCP_PRESETS)]


def render_mcp_preset_snippet(
    preset: McpServerPreset,
    server_name: str = "chrome",
) -> str:
    """Render a copy-pasteable YAML config snippet for a preset."""
    payload = {
        "mcp": {
            "servers": {
                server_name: {
                    "enabled": True,
                    "preset": preset.name,
                    "transport": preset.transport,
                    "command": preset.command,
                    "args": list(preset.args),
                    "capabilities": list(preset.capabilities),
                }
            }
        }
    }
    return yaml.safe_dump(payload, sort_keys=False, allow_unicode=True).strip()


def merge_preset_into_server_config(
    name: str,
    raw_value: dict[str, Any],
) -> McpServerConfig:
    """Merge builtin preset defaults with explicit server config values."""
    preset_name = str(raw_value.get("preset", "")).strip()
    preset = get_builtin_mcp_preset(preset_name)

    transport = str(raw_value.get("transport", "")).strip()
    if not transport:
        transport = preset.transport if preset else "stdio"

    command = str(raw_value.get("command", "")).strip()
    if not command and preset is not None:
        command = preset.command

    args = [str(arg) for arg in raw_value.get("args", []) or []]
    if not args and preset is not None:
        args = list(preset.args)

    env = {
        str(key): str(value) for key, value in (raw_value.get("env", {}) or {}).items()
    }
    if not env and preset is not None:
        env = dict(preset.env)

    cwd = str(raw_value.get("cwd", "")).strip()
    if not cwd and preset is not None:
        cwd = preset.cwd

    capabilities = [
        str(capability) for capability in raw_value.get("capabilities", []) or []
    ]
    if not capabilities and preset is not None:
        capabilities = list(preset.capabilities)

    action_map = dict(preset.default_action_map) if preset is not None else {}
    action_map.update(
        {
            str(action): str(tool_name)
            for action, tool_name in (raw_value.get("action_map", {}) or {}).items()
        }
    )

    return McpServerConfig(
        name=name,
        preset=preset_name,
        enabled=bool(raw_value.get("enabled", False)),
        transport=transport,
        command=command,
        args=args,
        env=env,
        cwd=cwd,
        capabilities=capabilities,
        action_map=action_map,
    )
