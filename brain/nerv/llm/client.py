"""Unified LLM client — one model call, dispatched by model-string prefix.

Model string scheme (the single switch point for swapping providers):
    "claude-cli:<alias>"   -> local Claude Code CLI (uses your Claude subscription)
    "local:<ollama-model>" -> local Ollama HTTP (reserved; dormant)
    "litellm:<model>"      -> litellm cloud API (reserved)
    bare cloud name        -> treated as litellm (e.g. "claude-opus-4-8")

`complete()` performs exactly ONE assistant turn and returns its content plus
any requested tool calls. The orchestrator owns the tool-execution loop for the
ollama/litellm providers. The claude-cli provider runs Claude Code's own agentic
loop internally (tools exposed via MCP), so it returns the final answer and sets
`runs_own_tool_loop=True` to tell the caller not to loop again.
"""

from __future__ import annotations

import json
import logging
import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import httpx

logger = logging.getLogger(__name__)

DEFAULT_OLLAMA_URL = "http://localhost:11434"

_CLOUD_PREFIXES = ("gpt-", "claude-", "gemini-", "o1-", "deepseek-")


@dataclass
class CompletionResult:
    """The outcome of a single model turn."""

    content: str
    tool_calls: list[dict[str, Any]] = field(default_factory=list)
    runs_own_tool_loop: bool = False
    # Optional observability metadata (populated by backends that expose it).
    provider: str = ""
    duration_ms: float | None = None
    cost_usd: float | None = None
    usage: dict[str, Any] | None = None


def parse_model(model: str) -> tuple[str, str]:
    """Split a model string into (provider, model_name).

    Returns one of providers: "claude-cli", "local", "litellm".
    """
    if model.startswith("claude-cli:"):
        return "claude-cli", model[len("claude-cli:") :]
    if model.startswith("local:"):
        return "local", model[len("local:") :]
    if model.startswith("litellm:"):
        return "litellm", model[len("litellm:") :]
    # Bare cloud names (e.g. "claude-opus-4-8") route through litellm for
    # backwards compatibility with the reserved API path.
    return "litellm", model


async def complete(
    messages: list[dict[str, Any]],
    *,
    model: str,
    tools: list[dict[str, Any]] | None = None,
    tool_names: list[str] | None = None,
    temperature: float = 0.2,
    json_mode: bool = False,
    project_root: Path | None = None,
    timeout: float = 120.0,
) -> CompletionResult:
    """Run one assistant turn against the configured backend.

    Args:
        messages: Chat messages (the first ``system`` message, if any, is the
            system prompt; the rest form the conversation).
        model: Provider-prefixed model string (see module docstring).
        tools: Function-call schemas for the ollama/litellm tool loop.
        tool_names: Resolved native tool names to expose to claude-cli via MCP.
        temperature: Sampling temperature for ollama/litellm.
        json_mode: Hint the model to emit strict JSON (used by the router).
        project_root: Project root, required for the claude-cli MCP bridge.
        timeout: Per-call timeout in seconds.
    """
    provider, model_name = parse_model(model)

    if provider == "claude-cli":
        from nerv.llm.claude_cli import complete_claude_cli

        return await complete_claude_cli(
            messages,
            model_name=model_name,
            tool_names=tool_names,
            json_mode=json_mode,
            project_root=project_root,
            timeout=timeout,
        )

    if provider == "local":
        return await _complete_ollama(
            messages,
            model_name=model_name,
            tools=tools,
            temperature=temperature,
            json_mode=json_mode,
            timeout=timeout,
        )

    return await _complete_litellm(
        messages,
        model_name=model_name,
        tools=tools,
        temperature=temperature,
        json_mode=json_mode,
    )


async def _complete_ollama(
    messages: list[dict[str, Any]],
    *,
    model_name: str,
    tools: list[dict[str, Any]] | None,
    temperature: float,
    json_mode: bool,
    timeout: float,
) -> CompletionResult:
    """Local Ollama HTTP backend (reserved; dormant by default)."""
    from nerv.hardware import PROFILE

    base_url = os.environ.get("NERV_OLLAMA_URL", DEFAULT_OLLAMA_URL)
    options: dict[str, Any] = {"temperature": temperature, "num_predict": 1024}
    options.update(PROFILE.get("ollama_options", {}))

    payload: dict[str, Any] = {
        "model": model_name,
        "messages": messages,
        "stream": False,
        "options": options,
    }
    if json_mode:
        payload["format"] = "json"
    if tools:
        payload["tools"] = tools

    async with httpx.AsyncClient(timeout=timeout) as client:
        response = await client.post(f"{base_url}/api/chat", json=payload)
        response.raise_for_status()
        data = response.json()

    message_obj = data.get("message", {})
    return CompletionResult(
        content=message_obj.get("content", "") or "",
        tool_calls=list(message_obj.get("tool_calls", []) or []),
    )


async def _complete_litellm(
    messages: list[dict[str, Any]],
    *,
    model_name: str,
    tools: list[dict[str, Any]] | None,
    temperature: float,
    json_mode: bool,
) -> CompletionResult:
    """Cloud API backend via litellm (reserved; dormant by default)."""
    import litellm

    kwargs: dict[str, Any] = {
        "model": model_name,
        "messages": messages,
        "temperature": temperature,
    }
    if json_mode:
        kwargs["response_format"] = {"type": "json_object"}
    if tools:
        kwargs["tools"] = [
            {"type": "function", "function": schema["function"]} for schema in tools
        ]

    resp = await litellm.acompletion(**kwargs)
    choice = resp.choices[0].message
    content = choice.content or ""

    tool_calls: list[dict[str, Any]] = []
    if getattr(choice, "tool_calls", None):
        for tc in choice.tool_calls:
            args_str = tc.function.arguments
            args_dict = json.loads(args_str) if isinstance(args_str, str) else args_str
            tool_calls.append(
                {"function": {"name": tc.function.name, "arguments": args_dict}}
            )

    return CompletionResult(content=content, tool_calls=tool_calls)
