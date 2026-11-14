"""One-line-per-request call log (``personal/logs/trace.jsonl``).

Privacy contract: a record holds only names, sizes, counts and categories --
never message text, model replies, tool arguments or results, or secrets. The
orchestrator fills a :class:`RequestTrace` while handling one request and writes
it once, after the reply is decided. Writing can never fail or delay a request.
"""

from __future__ import annotations

import contextlib
import contextvars
import fcntl
import json
import logging
import os
import time
from collections.abc import Iterator
from dataclasses import asdict, dataclass, field
from datetime import UTC, datetime
from pathlib import Path

logger = logging.getLogger(__name__)

TRACE_RELATIVE_PATH = Path("personal") / "logs" / "trace.jsonl"
TRACE_MAX_BYTES = 5 * 1024 * 1024
TRACE_KEEP_ROTATED = 3


@dataclass
class RequestTrace:
    """What is known about one request; serialised as one JSON line."""

    channel: str = ""
    scope: str = ""
    tier: int | None = None
    model: str = ""
    provider: str = ""
    est_input_tokens: int = 0
    tools_called: list[str] = field(default_factory=list)
    cost_usd: float | None = None
    pending_created: bool = False
    delegated: bool = False
    error: str = ""
    started: float = field(default_factory=time.perf_counter)

    def to_record(self) -> dict[str, object]:
        """Build the JSON record (fixed field set, no free text)."""
        data = asdict(self)
        started = data.pop("started")
        record: dict[str, object] = {
            "ts": datetime.now(UTC).isoformat(timespec="milliseconds"),
            **data,
            "duration_ms": round((time.perf_counter() - started) * 1000, 1),
            "error": self.error or None,
        }
        return record


_current: contextvars.ContextVar[RequestTrace | None] = contextvars.ContextVar(
    "nerv_request_trace", default=None
)


def current_trace() -> RequestTrace | None:
    """The trace of the request being handled, if any."""
    return _current.get()


@contextlib.contextmanager
def request_trace(channel: str, scope: str) -> Iterator[RequestTrace]:
    """Bind a fresh trace for the duration of one request."""
    trace = RequestTrace(channel=channel, scope=scope)
    token = _current.set(trace)
    try:
        yield trace
    finally:
        _current.reset(token)


def estimate_tokens(messages: list[dict[str, object]]) -> int:
    """Rough input size: characters / 4. Only lengths are used, never text."""
    total = sum(len(str(m.get("content") or "")) for m in messages)
    return total // 4


def _rotate(path: Path) -> None:
    """Shift ``trace.jsonl`` -> ``.1`` -> ``.2`` -> ``.3``, dropping the oldest."""
    for index in range(TRACE_KEEP_ROTATED, 0, -1):
        source = path if index == 1 else path.with_name(f"{path.name}.{index - 1}")
        target = path.with_name(f"{path.name}.{index}")
        if source.exists():
            os.replace(source, target)


def write_trace(project_root: Path, trace: RequestTrace) -> None:
    """Append the request's line; log a warning and return on any failure."""
    try:
        path = project_root / TRACE_RELATIVE_PATH
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        os.chmod(path.parent, 0o700)
        line = json.dumps(trace.to_record(), ensure_ascii=False) + "\n"
        lock_fd = os.open(path.parent / "trace.lock", os.O_WRONLY | os.O_CREAT, 0o600)
        try:
            fcntl.flock(lock_fd, fcntl.LOCK_EX)
            if path.exists() and path.stat().st_size >= TRACE_MAX_BYTES:
                _rotate(path)
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                handle.write(line)
        finally:
            os.close(lock_fd)
    except Exception as exc:  # noqa: BLE001 - tracing must never affect a request
        logger.warning("Could not write request trace: %s", type(exc).__name__)
