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

from nerv.orchestrator.orchestrator import Orchestrator
from nerv.router.router import Router

logger = logging.getLogger(__name__)

#: Upper bound on requests being handled at once.
MAX_CONCURRENT_REQUESTS = 8

_router: Router | None = None
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


async def _get_router() -> Router:
    """Return the shared Router, constructing it once even under concurrency."""
    global _router
    if _router is None:
        async with _get_init_lock():
            if _router is None:
                _router = Router(project_root=_get_project_root())
    return _router


async def _get_orchestrator() -> Orchestrator:
    """Return the shared Orchestrator, constructing it once even under concurrency."""
    global _orchestrator
    if _orchestrator is None:
        async with _get_init_lock():
            if _orchestrator is None:
                _orchestrator = Orchestrator(_get_project_root())
    return _orchestrator


async def handle_request(method: str, params: dict[str, Any] | None) -> Any:
    """Dispatch a JSON-RPC method call to the appropriate handler."""
    if method == "route":
        if params is None:
            raise ValueError("route method requires params")
        message = params.get("message", "")
        channel = params.get("channel", "")
        sender = params.get("sender", "")
        router = await _get_router()
        route_result = await router.classify(message)

        # Dispatch to orchestrator for actual agent response
        orchestrator = await _get_orchestrator()
        reply = await orchestrator.dispatch(
            message,
            route_result,
            channel=channel,
            sender=sender,
        )

        result = route_result.model_dump()
        result["reply"] = reply
        return result
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
    reader = asyncio.StreamReader()
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
            raw = await reader.readline()
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
