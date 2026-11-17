"""Lightweight structured observability for the brain.

Emits one JSON event per significant action (model call, turn, tool) to the
brain's logger (stderr) and, when ``NERV_TRACE_FILE`` is set, appends the same
JSON lines to that file. This is the substrate for debugging reliability issues
and, later, building an eval/metrics view — without pulling in a heavy tracing
dependency.
"""

from __future__ import annotations

import json
import logging
import os
import time
import uuid
from collections.abc import Iterator
from contextlib import contextmanager
from typing import Any

logger = logging.getLogger("nerv.trace")


def new_trace_id() -> str:
    """Return a short correlation id for one user turn."""
    return uuid.uuid4().hex[:12]


def log_event(event: str, **fields: Any) -> None:
    """Emit a single structured event as a JSON line."""
    record = {"event": event, "ts": round(time.time(), 3), **fields}
    line = json.dumps(record, ensure_ascii=False, default=str)
    logger.info(line)

    trace_file = os.environ.get("NERV_TRACE_FILE")
    if trace_file:
        try:
            with open(trace_file, "a", encoding="utf-8") as handle:
                handle.write(line + "\n")
        except OSError:
            logger.debug("Failed to append trace to %s", trace_file, exc_info=True)


@contextmanager
def timed(event: str, **fields: Any) -> Iterator[dict[str, Any]]:
    """Time a block and emit a structured event with duration and outcome.

    Yields a mutable dict so callers can attach extra fields discovered during
    the block (e.g. token usage). On exception, logs the failure and re-raises.
    """
    extra: dict[str, Any] = {}
    start = time.perf_counter()
    try:
        yield extra
    except Exception as exc:
        duration_ms = round((time.perf_counter() - start) * 1000, 1)
        log_event(
            event,
            ok=False,
            duration_ms=duration_ms,
            error=f"{type(exc).__name__}: {exc}",
            **fields,
            **extra,
        )
        raise
    else:
        duration_ms = round((time.perf_counter() - start) * 1000, 1)
        log_event(event, ok=True, duration_ms=duration_ms, **fields, **extra)
