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

from nerv.models import RouteResult
from nerv.orchestrator.orchestrator import Orchestrator

logger = logging.getLogger(__name__)

#: Longest single JSON-RPC line accepted from stdin (asyncio's default is 64 KiB).
MAX_LINE_BYTES = 64 * 1024 * 1024

#: Upper bound on requests being handled at once.
MAX_CONCURRENT_REQUESTS = 8

_orchestrator: Orchestrator | None = None
_init_lock: asyncio.Lock | None = None
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


def _get_init_lock() -> asyncio.Lock:
    global _init_lock
    if _init_lock is None:
        _init_lock = asyncio.Lock()
    return _init_lock


async def _get_orchestrator() -> Orchestrator:
    """Return the shared Orchestrator, constructing it once even under concurrency."""
    global _orchestrator
    if _orchestrator is None:
        async with _get_init_lock():
            if _orchestrator is None:
                _orchestrator = Orchestrator(_get_project_root())
    return _orchestrator


async def handle_request(method: str, params: dict[str, Any] | None) -> Any:
    """Dispatch a JSON-RPC method call to the appropriate handler.

    Since the router and the turn loop migrated to Rust (``core/brain``),
    the old ``route`` method is split: the host classifies locally and drives
    this brain with ``check_pending`` (confirm/cancel <id>, before any model
    call) and the turn's leaf operations — ``prepare_turn`` (agent selection,
    context assembly), ``run_tool`` (capability gate + execution),
    ``complete_turn`` (python-only backends), ``finish_turn`` (memory append,
    pending surfacing, summaries).
    """
    if method == "check_pending":
        if params is None:
            raise ValueError("check_pending method requires params")
        message = params.get("message", "")
        channel = params.get("channel", "")
        sender = params.get("sender", "")
        orchestrator = await _get_orchestrator()

        # confirm/cancel <id> is handled before classification, so approving a
        # queued action never depends on a model call succeeding.
        pending_reply = await orchestrator.dispatch_pending_command(
            message, channel=channel, sender=sender
        )
        return {"pending": pending_reply is not None, "reply": pending_reply or ""}
    elif method == "prepare_turn":
        if params is None:
            raise ValueError("prepare_turn method requires params")
        message = params.get("message", "")
        channel = params.get("channel", "")
        sender = params.get("sender", "")
        route_result = RouteResult(**(params.get("route_result") or {}))
        orchestrator = await _get_orchestrator()
        return await orchestrator.prepare_turn(
            message, route_result, channel=channel, sender=sender
        )
    elif method == "run_tool":
        if params is None:
            raise ValueError("run_tool method requires params")
        orchestrator = await _get_orchestrator()
        return await orchestrator.run_tool(
            params.get("name", ""),
            params.get("arguments", {}),
            sender=params.get("sender", ""),
            channel=params.get("channel", ""),
            scope=params.get("scope", ""),
        )
    elif method == "finish_turn":
        if params is None:
            raise ValueError("finish_turn method requires params")
        orchestrator = await _get_orchestrator()
        return await orchestrator.finish_turn(
            params.get("reply", ""),
            channel=params.get("channel", ""),
            sender=params.get("sender", ""),
            known_pending=params.get("known_pending") or [],
            notifications=params.get("notifications") or [],
        )
    elif method == "complete_turn":
        if params is None:
            raise ValueError("complete_turn method requires params")
        orchestrator = await _get_orchestrator()
        return await orchestrator.complete_turn(
            params.get("messages", []),
            params.get("model", ""),
            tools=params.get("tools") or None,
            scope=params.get("scope", ""),
        )
    elif method == "browser_mcp_attempt":
        if params is None:
            raise ValueError("browser_mcp_attempt method requires params")
        orchestrator = await _get_orchestrator()
        return await orchestrator.browser_mcp_attempt(
            params.get("action", ""),
            params.get("arguments", {}),
        )
    elif method == "heartbeat":
        orchestrator = await _get_orchestrator()
        reply = await orchestrator.check_background_tasks()
        return {"reply": reply or ""}
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


async def _shutdown() -> None:
    """Release resources held by the long-lived singletons."""
    global _orchestrator
    if _orchestrator is not None:
        try:
            await _orchestrator.aclose()
        except Exception:
            logger.debug("Orchestrator shutdown failed", exc_info=True)
        _orchestrator = None


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
        await _shutdown()
