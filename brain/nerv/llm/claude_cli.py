"""Claude Code CLI backend — runs prompts through the local `claude` binary.

This uses the user's logged-in Claude Code session (subscription auth), so no
ANTHROPIC_API_KEY is required. Claude Code runs its own agentic tool loop; Nerv
native tools are exposed to it through an MCP server (see nerv.mcp_server), and
high-risk / out-of-sandbox actions are gated there via the confirmation queue.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import tempfile
from pathlib import Path
from typing import Any

from nerv.llm.client import CompletionResult
from nerv.observability import log_event

logger = logging.getLogger(__name__)

CLAUDE_BIN = os.environ.get("NERV_CLAUDE_BIN", "claude")
MCP_SERVER_NAME = "nerv"

# Claude Code's own built-in tools. We disable them so it behaves as a pure
# model and only acts through Nerv's MCP-exposed tools.
_CC_BUILTIN_TOOLS = [
    "Bash",
    "Edit",
    "MultiEdit",
    "Write",
    "Read",
    "Glob",
    "Grep",
    "LS",
    "WebFetch",
    "WebSearch",
    "Task",
    "TodoWrite",
    "NotebookEdit",
    "BashOutput",
    "KillShell",
]


def _flatten_messages(messages: list[dict[str, Any]]) -> tuple[str, str]:
    """Split chat messages into (system_prompt, conversation_prompt)."""
    system_parts: list[str] = []
    convo: list[str] = []
    for msg in messages:
        role = msg.get("role", "")
        content = msg.get("content", "")
        if not isinstance(content, str):
            content = json.dumps(content, ensure_ascii=False)
        if role == "system":
            if content.strip():
                system_parts.append(content.strip())
        elif role == "assistant":
            if content.strip():
                convo.append(f"Assistant: {content.strip()}")
        elif role == "tool":
            convo.append(f"[Tool result]: {content.strip()}")
        else:  # user (and any unknown roles)
            convo.append(content.strip())

    system_prompt = "\n\n".join(system_parts)
    # If there is no prior history, the single user message is the whole prompt.
    conversation = "\n\n".join(part for part in convo if part)
    return system_prompt, conversation


def _resolve_provider_names(tool_names: list[str]) -> list[str]:
    """Map requested tool/capability names to concrete provider tool names."""
    from nerv.capabilities import capability_registry

    resolved: list[str] = []
    seen: set[str] = set()
    for name in tool_names:
        resolution = capability_registry.resolve(name)
        provider = resolution.provider_name if resolution else name
        if provider not in seen:
            resolved.append(provider)
            seen.add(provider)
    return resolved


def _write_mcp_config(project_root: Path) -> Path:
    """Write a temp MCP config that points Claude at the Nerv tool server."""
    config = {
        "mcpServers": {
            MCP_SERVER_NAME: {
                "command": "uv",
                "args": ["run", "python", "-m", "nerv.mcp_server"],
                "cwd": str(project_root / "brain"),
                "env": {"NERV_PROJECT_ROOT": str(project_root)},
            }
        }
    }
    fd, path = tempfile.mkstemp(prefix="nerv-mcp-", suffix=".json")
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(config, handle)
    return Path(path)


async def complete_claude_cli(
    messages: list[dict[str, Any]],
    *,
    model_name: str,
    tool_names: list[str] | None = None,
    json_mode: bool = False,
    project_root: Path | None = None,
    timeout: float = 120.0,
) -> CompletionResult:
    """Run one turn through the `claude` CLI in print mode."""
    system_prompt, conversation = _flatten_messages(messages)
    if json_mode:
        system_prompt = (
            f"{system_prompt}\n\nRespond with ONLY a single valid JSON object and "
            "no other text."
        ).strip()

    args: list[str] = [
        CLAUDE_BIN,
        "-p",
        "--model",
        model_name or "opus",
        "--output-format",
        "json",
        "--disallowedTools",
        *_CC_BUILTIN_TOOLS,
    ]
    if system_prompt:
        args += ["--system-prompt", system_prompt]

    mcp_config_path: Path | None = None
    provider_names = _resolve_provider_names(tool_names) if tool_names else []
    if provider_names and project_root is not None:
        mcp_config_path = _write_mcp_config(project_root)
        allowed = [f"mcp__{MCP_SERVER_NAME}__{name}" for name in provider_names]
        args += [
            "--mcp-config",
            str(mcp_config_path),
            "--strict-mcp-config",
            "--allowedTools",
            *allowed,
        ]

    logger.debug("Invoking claude CLI: model=%s tools=%s", model_name, provider_names)
    attempts = max(1, int(os.environ.get("NERV_CLAUDE_MAX_ATTEMPTS", "2")))
    last_error = "unknown error"
    try:
        for attempt in range(1, attempts + 1):
            try:
                data = await _invoke_once(args, conversation, timeout)
            except RuntimeError as exc:
                last_error = str(exc)
                logger.warning(
                    "claude CLI attempt %d/%d failed: %s", attempt, attempts, exc
                )
                if attempt < attempts:
                    await asyncio.sleep(_backoff_seconds(attempt))
                continue

            if data.get("is_error") and attempt < attempts:
                last_error = f"claude returned error result: {data.get('subtype')}"
                logger.warning(
                    "claude CLI attempt %d/%d returned an error result",
                    attempt,
                    attempts,
                )
                await asyncio.sleep(_backoff_seconds(attempt))
                continue

            log_event(
                "model_call",
                provider="claude-cli",
                model=model_name or "opus",
                tools=len(provider_names),
                attempt=attempt,
                duration_ms=data.get("duration_ms"),
                cost_usd=data.get("total_cost_usd"),
                num_turns=data.get("num_turns"),
            )
            # Claude Code already executed any tool calls internally via MCP, so
            # the caller must not run its own tool loop on top of this.
            return CompletionResult(
                content=data.get("result", "") or "",
                runs_own_tool_loop=True,
                provider="claude-cli",
                duration_ms=data.get("duration_ms"),
                cost_usd=data.get("total_cost_usd"),
                usage=data.get("usage"),
            )

        raise RuntimeError(
            f"claude CLI failed after {attempts} attempt(s): {last_error}"
        )
    finally:
        if mcp_config_path is not None:
            mcp_config_path.unlink(missing_ok=True)


def _backoff_seconds(attempt: int) -> float:
    """Exponential backoff capped at 8s: 1s, 2s, 4s, 8s, ..."""
    return float(min(2 ** (attempt - 1), 8))


async def _invoke_once(
    args: list[str], conversation: str, timeout: float
) -> dict[str, Any]:
    """Run the claude CLI once; raise RuntimeError on any transient failure."""
    try:
        proc = await asyncio.create_subprocess_exec(
            *args,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout, stderr = await asyncio.wait_for(
            proc.communicate(conversation.encode("utf-8")),
            timeout=timeout,
        )
    except TimeoutError as exc:
        raise RuntimeError(f"claude CLI timed out after {timeout}s") from exc

    if proc.returncode != 0:
        err = stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"claude CLI exited {proc.returncode}: {err[:500]}")

    raw = stdout.decode("utf-8", errors="replace").strip()
    try:
        return json.loads(raw)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"claude CLI returned non-JSON output: {raw[:500]}") from exc
