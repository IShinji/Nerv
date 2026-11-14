"""Shared constants and low-level helpers for the builtin tools.

Path resolution, output truncation, URL normalization — the plumbing more than
one tool module needs. Helpers used by a single module live with that module
instead, so its tools keep calling them through their own namespace.
"""

import logging
import os
import pathlib
import secrets
import stat
import tempfile
import time
from urllib.parse import urlparse

logger = logging.getLogger(__name__)

TIMEZONE_ALIASES = {
    "utc": "UTC",
    "gmt": "UTC",
    "seattle": "America/Los_Angeles",
    "los angeles": "America/Los_Angeles",
    "san francisco": "America/Los_Angeles",
    "new york": "America/New_York",
    "beijing": "Asia/Shanghai",
    "shanghai": "Asia/Shanghai",
    "tokyo": "Asia/Tokyo",
    "london": "Europe/London",
}
DEFAULT_HTTP_TIMEOUT = 20.0
MAX_TOOL_OUTPUT_CHARS = 5000
# Oversized tool output is written in full to a spill file; the model only sees
# a head/tail preview plus the path (readable with the read_file tool).
SPILL_PREVIEW_HEAD_CHARS = 3000
SPILL_PREVIEW_TAIL_CHARS = 1000
SPILL_RETENTION_DAYS = 7
SPILL_DIR_ENV = "NERV_SPILL_DIR"
SPILL_FILE_PREFIX = "spill-"
SPILL_FILE_SUFFIX = ".txt"
SEARCH_ENDPOINT = "https://html.duckduckgo.com/html/"


def _resolve_path(path: str) -> pathlib.Path:
    """Resolve a path for tool access."""
    candidate = pathlib.Path(path).expanduser()
    if candidate.is_absolute():
        return candidate
    return (_get_project_root() / candidate).resolve()


def _get_project_root() -> pathlib.Path:
    """Find the Nerv project root from the current working directory."""
    env_root = os.environ.get("NERV_PROJECT_ROOT")
    if env_root:
        return pathlib.Path(env_root).expanduser().resolve()
    current = pathlib.Path.cwd().resolve()
    for candidate in [current, *current.parents]:
        if (candidate / "agents").exists() or (candidate / "core").exists():
            return candidate
    return current


def _spill_dir() -> pathlib.Path:
    """Directory for spill files: readable by ``read_file``, never a secret path.

    ``personal/`` (including ``personal/workspace/``) is a secret path for the
    file tools, so spill files live in a per-user directory under the system
    temp dir instead. ``NERV_SPILL_DIR`` overrides it.
    """
    override = os.environ.get(SPILL_DIR_ENV, "").strip()
    if override:
        return pathlib.Path(override).expanduser()
    return pathlib.Path(tempfile.gettempdir()) / f"nerv-spill-{os.getuid()}"


def _prepare_spill_dir(directory: pathlib.Path) -> None:
    """Create ``directory`` (0700) and verify it is ours and not a symlink."""
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = directory.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid():
        raise OSError("spill directory is not a private directory owned by us")
    if info.st_mode & 0o077:
        directory.chmod(0o700)


def _cleanup_spill_files(directory: pathlib.Path, now: float) -> None:
    """Delete spill files older than the retention window (own naming only)."""
    cutoff = now - SPILL_RETENTION_DAYS * 86400
    for entry in directory.iterdir():
        name = entry.name
        if not (
            name.startswith(SPILL_FILE_PREFIX) and name.endswith(SPILL_FILE_SUFFIX)
        ):
            continue
        try:
            info = entry.lstat()
            if stat.S_ISREG(info.st_mode) and info.st_mtime < cutoff:
                entry.unlink()
        except OSError:
            continue


def _spill_to_file(text: str, tool_name: str) -> pathlib.Path:
    """Write the full text to a fresh 0600 file and return its path."""
    directory = _spill_dir()
    _prepare_spill_dir(directory)
    now = time.time()
    try:
        _cleanup_spill_files(directory, now)
    except OSError:
        logger.warning("Spill cleanup failed", exc_info=True)
    safe_tool = "".join(c if c.isalnum() else "_" for c in tool_name)[:40] or "tool"
    stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime(now))
    name = f"{SPILL_FILE_PREFIX}{safe_tool}-{stamp}-{secrets.token_hex(4)}"
    target = directory / (name + SPILL_FILE_SUFFIX)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(target, flags, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        handle.write(text)
    return target


def _truncate(
    text: str, limit: int = MAX_TOOL_OUTPUT_CHARS, tool_name: str = "tool"
) -> str:
    """Shrink large tool output for context safety, spilling the full text.

    Over the limit, the full text is saved to a private file and the caller gets
    a head and tail preview plus the omitted size and the file path. If the file
    cannot be written the preview is still returned, with a note saying so.
    """
    if len(text) <= limit:
        return text
    head_len = min(SPILL_PREVIEW_HEAD_CHARS, limit * 3 // 4)
    tail_len = min(SPILL_PREVIEW_TAIL_CHARS, max(limit - head_len, 0))
    omitted = len(text) - head_len - tail_len
    head = text[:head_len]
    tail = text[len(text) - tail_len :] if tail_len else ""
    try:
        spill_path = _spill_to_file(text, tool_name)
        note = (
            f"[{omitted} characters omitted. Full output ({len(text)} characters) "
            f"saved to {spill_path} - use read_file to retrieve it]"
        )
    except OSError as exc:
        logger.warning("Could not spill tool output: %s", exc)
        note = f"[{omitted} characters omitted; saving the full output failed]"
    return f"{head}\n...\n{note}\n...\n{tail}"


def _parse_csv_or_lines(value: str) -> list[str]:
    """Parse comma- or newline-separated text into a clean string list."""
    if not value.strip():
        return []
    normalized = value.replace("\r", "\n").replace(",", "\n")
    return [item.strip() for item in normalized.split("\n") if item.strip()]


def _ensure_url(target_url: str) -> str:
    """Normalize a user URL into an absolute browser URL."""
    normalized = target_url.strip()
    if not normalized:
        return normalized
    if not urlparse(normalized).scheme:
        normalized = "https://" + normalized
    return normalized
