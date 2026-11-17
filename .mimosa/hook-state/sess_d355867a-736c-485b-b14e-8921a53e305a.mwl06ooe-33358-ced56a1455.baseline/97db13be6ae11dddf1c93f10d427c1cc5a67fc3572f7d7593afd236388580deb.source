"""The pending action a user has just confirmed, visible to the code that runs it.

A tool that is gated one call at a time can use this to recognise that the call it
is in the middle of was approved as a whole. It is set only while the orchestrator
runs a confirmed action, and each reader decides for itself whether the action it
sees is the one it was approved for (tool, arguments and the plan the user was
shown must all match), so confirming one action never approves another.
"""

from __future__ import annotations

from collections.abc import Iterator
from contextlib import contextmanager
from contextvars import ContextVar

from nerv.security.pending import PendingRecord

_confirmed: ContextVar[PendingRecord | None] = ContextVar(
    "nerv_confirmed_action", default=None
)


def confirmed_action() -> PendingRecord | None:
    """The pending action being run because the user confirmed it, if any."""
    return _confirmed.get()


@contextmanager
def executing_confirmed(action: PendingRecord) -> Iterator[None]:
    """Mark ``action`` as the confirmed action for everything run in the block."""
    token = _confirmed.set(action)
    try:
        yield
    finally:
        _confirmed.reset(token)
