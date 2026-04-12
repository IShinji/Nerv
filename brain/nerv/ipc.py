"""JSON-RPC 2.0 server over stdin/stdout.

Protocol: newline-delimited JSON. Each message is one line.
stderr is used for logging — never write non-JSON to stdout.
"""

import json
import logging
import os
import sys
from pathlib import Path
from typing import Any

from nerv.models import RouteResult
from nerv.orchestrator.orchestrator import Orchestrator
from nerv.router.router import Router

logger = logging.getLogger(__name__)

_router: Router | None = None
_orchestrator: Orchestrator | None = None


def _get_project_root() -> Path:
    """Determine project root by walking up from brain/ directory."""
    brain_dir = Path(__file__).resolve().parent.parent
    # brain/ is at project_root/brain, so go up one more
    project_root = brain_dir.parent
    if (project_root / "agents").exists() or (project_root / "core").exists():
        return project_root
    # Fallback: use cwd
    return Path.cwd().parent if Path.cwd().name == "brain" else Path.cwd()


def _get_router() -> Router:
    global _router
    if _router is None:
        _router = Router()
    return _router


def _get_orchestrator() -> Orchestrator:
    global _orchestrator
    if _orchestrator is None:
        project_root = _get_project_root()
        _orchestrator = Orchestrator(project_root)
    return _orchestrator


async def handle_request(method: str, params: dict[str, Any] | None) -> Any:
    """Dispatch a JSON-RPC method call to the appropriate handler."""
    if method == "route":
        if params is None:
            raise ValueError("route method requires params")
        message = params.get("message", "")
        channel = params.get("channel", "")
        sender = params.get("sender", "")
        router = _get_router()
        route_result = await router.classify(message)

        # Dispatch to orchestrator for actual agent response
        orchestrator = _get_orchestrator()
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
        orchestrator = _get_orchestrator()
        import inspect
        if inspect.iscoroutinefunction(orchestrator.check_background_tasks):
            reply = await orchestrator.check_background_tasks()
        else:
            reply = orchestrator.check_background_tasks()
        
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


def make_success(result: Any, request_id: int | str) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "result": result, "id": request_id}


def make_error(
    code: int, message: str, request_id: int | str | None
) -> dict[str, Any]:
    return {
        "jsonrpc": "2.0",
        "error": {"code": code, "message": message},
        "id": request_id,
    }


async def run_jsonrpc_loop() -> None:
    """Main loop: read JSON-RPC requests from stdin, process, write responses."""
    logger.info("JSON-RPC loop started, waiting for requests on stdin...")

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        logger.debug("← Received: %s", line)

        try:
            request = json.loads(line)
        except json.JSONDecodeError as e:
            write_response(make_error(-32700, f"Parse error: {e}", None))
            continue

        request_id = request.get("id")
        method = request.get("method")
        params = request.get("params")

        if not method:
            write_response(make_error(-32600, "Invalid request: missing method", request_id))
            continue

        try:
            result = await handle_request(method, params)
            write_response(make_success(result, request_id))
        except Exception as e:
            logger.exception("Error handling method %s", method)
            write_response(make_error(-32000, str(e), request_id))
