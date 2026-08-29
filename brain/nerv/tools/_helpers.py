"""Shared constants and low-level helpers for the builtin tools.

Path resolution, output truncation, URL normalization — the plumbing more than
one tool module needs. Helpers used by a single module live with that module
instead, so its tools keep calling them through their own namespace.
"""

import logging
import pathlib
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
SEARCH_ENDPOINT = "https://html.duckduckgo.com/html/"


def _resolve_path(path: str) -> pathlib.Path:
    """Resolve a path for tool access."""
    candidate = pathlib.Path(path).expanduser()
    if candidate.is_absolute():
        return candidate
    return (_get_project_root() / candidate).resolve()


def _get_project_root() -> pathlib.Path:
    """Find the Nerv project root from the current working directory."""
    current = pathlib.Path.cwd().resolve()
    for candidate in [current, *current.parents]:
        if (candidate / "agents").exists() or (candidate / "core").exists():
            return candidate
    return current


def _truncate(text: str, limit: int = MAX_TOOL_OUTPUT_CHARS) -> str:
    """Truncate large tool output for context safety."""
    if len(text) <= limit:
        return text
    return text[:limit] + "\n...[truncated]"


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
