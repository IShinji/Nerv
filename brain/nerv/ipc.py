"""JSON-RPC 2.0 server over stdin/stdout.

Protocol: newline-delimited JSON. Each message is one line.
stderr is used for logging — never write non-JSON to stdout.

The loop is genuinely asynchronous: stdin is drained through an asyncio pipe
reader and each request is handled in its own task, so a long model call for
one message no longer blocks the heartbeat or a second channel's message.
Responses may therefore come back out of order — every reply carries the
request ``id``, which is what the Rust gateway matches on. Concurrency is
capped by :data:`MAX_CONCURRENT_REQUESTS` so a burst cannot spawn unbounded
model calls.
"""

import asyncio
import json
import logging
import sys
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)

#: Longest single JSON-RPC line accepted from stdin (asyncio's default is 64 KiB).
MAX_LINE_BYTES = 64 * 1024 * 1024

#: Upper bound on requests being handled at once.
MAX_CONCURRENT_REQUESTS = 8

_stdout_lock: asyncio.Lock | None = None


def _get_project_root() -> Path:
    """Determine project root by walking up from brain/ directory."""
    brain_dir = Path(__file__).resolve().parent.parent
    # brain/ is at project_root/brain, so go up one more
    project_root = brain_dir.parent
    if (project_root / "agents").exists() or (project_root / "core").exists():
        return project_root
    # Fallback: use cwd
    return Path.cwd().parent if Path.cwd().name == "brain" else Path.cwd()


async def _complete_turn(
    messages: list[dict[str, Any]],
    model: str,
    tools: list[dict[str, Any]] | None,
) -> dict[str, Any]:
    """One python llm call for the backends the Rust client doesn't serve.

    Only the litellm path routes here (a python-only dependency); the
    claude-cli and local providers run in Rust. Python loads the user's
    personal API keys before calling cloud providers.
    """
    env_path = _get_project_root() / "personal" / "api_keys.env"
    if env_path.exists():
        from dotenv import load_dotenv

        load_dotenv(env_path)

    from nerv.llm import complete

    result = await complete(messages, model=model, tools=tools)
    return {
        "content": result.content,
        "tool_calls": result.tool_calls,
        "runs_own_tool_loop": result.runs_own_tool_loop,
        "provider": result.provider,
        "duration_ms": result.duration_ms,
        "cost_usd": result.cost_usd,
        "usage": result.usage,
    }


async def handle_request(method: str, params: dict[str, Any] | None) -> Any:
    """Dispatch a JSON-RPC method call to the appropriate handler.

    Nearly everything lives in Rust (``core/brain``): the router, the whole
    turn flow, the tool registry, memory, workflows, the MCP stack and the
    proactive heartbeat. This sidecar remains only for the litellm cloud
    backends — a python-only dependency the Rust client does not serve.
    """
    if method == "complete_turn":
        if params is None:
            raise ValueError("complete_turn method requires params")
        return await _complete_turn(
            params.get("messages", []),
            params.get("model", ""),
            params.get("tools") or None,
        )
    elif method == "ping":
        return {"status": "ok"}
    else:
        raise ValueError(f"Unknown method: {method}")


def write_response(response: dict[str, Any]) -> None:
    """Write a JSON-RPC response to stdout (one line)."""
    line = json.dumps(response, ensure_ascii=False)
    sys.stdout.write(line + "\n")
    sys.stdout.flush()


def write_notification(params: dict[str, Any]) -> None:
    """Emit a JSON-RPC notification (no id) — the E0 定向通知 channel.

    The gateway routes `notify{channel, sender, text}` to the matching
    adapter's per-sender delivery instead of broadcasting. Must only be
    called from the brain process (stdout is the JSON-RPC channel); writes
    are serialized by the caller or the stdout lock.
    """
    line = json.dumps(
        {"jsonrpc": "2.0", "method": "notify", "params": params},
        ensure_ascii=False,
    )
    sys.stdout.write(line + "\n")
    sys.stdout.flush()


async def write_response_async(response: dict[str, Any]) -> None:
    """Write one response, serialized against other in-flight handlers."""
    global _stdout_lock
    if _stdout_lock is None:
        _stdout_lock = asyncio.Lock()
    async with _stdout_lock:
        write_response(response)


def make_success(result: Any, request_id: int | str) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "result": result, "id": request_id}


def make_error(code: int, message: str, request_id: int | str | None) -> dict[str, Any]:
    return {
        "jsonrpc": "2.0",
        "error": {"code": code, "message": message},
        "id": request_id,
    }


async def _connect_stdin() -> asyncio.StreamReader:
    """Wrap stdin in an asyncio reader so the event loop stays responsive."""
    loop = asyncio.get_running_loop()
    reader = asyncio.StreamReader(limit=MAX_LINE_BYTES)
    protocol = asyncio.StreamReaderProtocol(reader)
    await loop.connect_read_pipe(lambda: protocol, sys.stdin)
    return reader


async def _process_request(
    method: str,
    params: dict[str, Any] | None,
    request_id: int | str | None,
    semaphore: asyncio.Semaphore,
) -> None:
    """Run one request to completion and write its response."""
    async with semaphore:
        try:
            result = await handle_request(method, params)
            await write_response_async(make_success(result, request_id))
        except Exception as e:
            logger.exception("Error handling method %s", method)
            await write_response_async(make_error(-32000, str(e), request_id))


async def run_jsonrpc_loop() -> None:
    """Main loop: read JSON-RPC requests from stdin, process, write responses."""
    logger.info("JSON-RPC loop started, waiting for requests on stdin...")

    reader = await _connect_stdin()
    semaphore = asyncio.Semaphore(MAX_CONCURRENT_REQUESTS)
    in_flight: set[asyncio.Task[None]] = set()

    try:
        while True:
            try:
                raw = await reader.readline()
            except (ValueError, asyncio.LimitOverrunError) as e:
                # Oversize line: the reader already discarded it; report and go on.
                await write_response_async(
                    make_error(-32600, f"Request too large: {e}", None)
                )
                continue
            if not raw:  # EOF — the gateway closed our stdin
                logger.info("stdin closed, shutting down JSON-RPC loop")
                break

            line = raw.decode("utf-8", errors="replace").strip()
            if not line:
                continue

            logger.debug("← Received: %s", line)

            try:
                request = json.loads(line)
            except json.JSONDecodeError as e:
                await write_response_async(
                    make_error(-32700, f"Parse error: {e}", None)
                )
                continue

            if not isinstance(request, dict):
                await write_response_async(
                    make_error(-32600, "Invalid request: not an object", None)
                )
                continue

            request_id = request.get("id")
            method = request.get("method")
            params = request.get("params")

            if not method:
                await write_response_async(
                    make_error(-32600, "Invalid request: missing method", request_id)
                )
                continue

            task = asyncio.create_task(
                _process_request(method, params, request_id, semaphore)
            )
            in_flight.add(task)
            task.add_done_callback(in_flight.discard)
    finally:
        if in_flight:
            logger.info("Draining %d in-flight request(s)", len(in_flight))
            await asyncio.gather(*in_flight, return_exceptions=True)
