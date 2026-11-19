"""Traversal-proof file writes and locks.

Review ids, slugs and lock names land in filesystem paths. Wherever the
filename is a bare constant (or a validated slug) the safe pattern is to
open the *directory* itself, then create the file relative to that
directory fd with ``O_NOFOLLOW`` — a traversing path or a symlink swap
can never move the write outside the intended directory, and there is no
resolve-then-open TOCTOU window.
"""

from __future__ import annotations

import fcntl
import os
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import IO


def _checked_bare_name(filename: str) -> str:
    """Reject anything that is not a plain file name (no separators, no ``..``)."""
    if (
        not filename
        or os.path.basename(filename) != filename
        or filename in (".", "..")
    ):
        raise ValueError(f"unsafe file name: {filename!r}")
    return filename


def _open_in_dir(directory: Path, filename: str) -> int:
    """Create/truncate ``filename`` inside ``directory`` and return its fd."""
    _checked_bare_name(filename)
    dir_fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
    try:
        return os.open(
            filename,
            os.O_WRONLY | os.O_CREAT | os.O_TRUNC,
            0o600,
            dir_fd=dir_fd,
        )
    finally:
        os.close(dir_fd)


@contextmanager
def open_write_in_dir(directory: Path, filename: str) -> Iterator[IO[str]]:
    """Open ``filename`` inside ``directory`` for text writing, safely."""
    fd = _open_in_dir(directory, filename)
    handle = None
    try:
        handle = os.fdopen(fd, "w", encoding="utf-8")
        yield handle
    finally:
        if handle is not None:
            handle.close()
        else:
            os.close(fd)  # fdopen itself failed — close the raw fd


@contextmanager
def exclusive_lock(directory: Path, lock_name: str) -> Iterator[None]:
    """Hold an exclusive cross-process flock on ``lock_name`` in ``directory``."""
    fd = _open_in_dir(directory, lock_name)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(fd, fcntl.LOCK_UN)
    finally:
        os.close(fd)
