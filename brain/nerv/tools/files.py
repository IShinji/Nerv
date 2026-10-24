"""Filesystem tools: read, list, search, and overwrite files."""

import logging
import pathlib
import subprocess

from nerv.security.sandbox import PROTECTED_FILE_NAMES, is_secret_path
from nerv.tools._helpers import (
    _get_project_root,
    _resolve_path,
)
from nerv.tools.registry import ToolResult, registry

logger = logging.getLogger(__name__)


GREP_TIMEOUT_SECONDS = 20


def _deny_secret(path: str) -> ToolResult | None:
    """Return an error result if ``path`` is a credentials/config location."""
    if path and is_secret_path(path, _get_project_root()):
        return ToolResult(
            content="Error: access to config and secret files is not allowed.",
            is_error=True,
        )
    return None


@registry.register(
    name="file_io",
    description=(
        "Inspect files and directories. operation can be read, list, or search. "
        "For writes, use write_file_full because writes are higher risk."
    ),
    requires_confirmation=False,
)
def file_io(
    operation: str,
    path: str,
    query: str = "",
    limit: int = 50,
) -> ToolResult:
    """Unified read/list/search file tool."""
    target = _resolve_path(path)
    if (denied := _deny_secret(str(target))) is not None:
        return denied
    normalized_op = operation.strip().lower()

    if normalized_op == "read":
        return read_file(str(target))

    if normalized_op == "list":
        if not target.exists():
            return ToolResult(
                content=f"Error: Path {target} does not exist.", is_error=True
            )
        if not target.is_dir():
            return ToolResult(
                content=f"Error: {target} is not a directory.", is_error=True
            )

        entries = sorted(
            target.iterdir(), key=lambda item: (item.is_file(), item.name.lower())
        )
        rendered = []
        for entry in entries[: max(1, limit)]:
            kind = "dir" if entry.is_dir() else "file"
            rendered.append(f"[{kind}] {entry.name}")

        if len(entries) > limit:
            rendered.append("...[truncated]")

        return ToolResult(content=f"Listing for {target}:\n" + "\n".join(rendered))

    if normalized_op == "search":
        if not query.strip():
            return ToolResult(
                content="Error: search requires a non-empty query.", is_error=True
            )
        return grep_search(str(target), query)

    if normalized_op in {"write", "append", "delete", "move"}:
        return ToolResult(
            content=(
                f"Unsupported file_io operation '{operation}'. "
                "Use write_file_full for writes because write operations require confirmation."
            ),
            is_error=True,
        )

    return ToolResult(
        content=f"Unsupported file_io operation '{operation}'. Use read, list, or search.",
        is_error=True,
    )


@registry.register(
    name="read_file",
    description="Read the exact contents of a file at a given absolute path.",
    requires_confirmation=False,
)
def read_file(absolute_path: str) -> ToolResult:
    """Read a file."""
    if (denied := _deny_secret(absolute_path)) is not None:
        return denied
    path = pathlib.Path(absolute_path)
    if not path.exists():
        return ToolResult(
            content=f"Error: File {absolute_path} does not exist.", is_error=True
        )
    if not path.is_file():
        return ToolResult(
            content=f"Error: {absolute_path} is not a file.", is_error=True
        )

    try:
        content = path.read_text(encoding="utf-8")
        return ToolResult(content=content)
    except Exception as e:
        return ToolResult(content=f"Error reading file: {e}", is_error=True)


@registry.register(
    name="write_file_full",
    description="Overwrite a file entirely with new content. Use carefully as this deletes old content.",
    # Path-gated by security.sandbox (outside the workspace or protected paths
    # are confirmed), so the blanket flag stays off for ordinary workspace files.
    requires_confirmation=False,
)
def write_file_full(absolute_path: str, content: str) -> ToolResult:
    """Overwrite a file entirely."""
    path = pathlib.Path(absolute_path)
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        return ToolResult(content=f"Successfully completely wrote to {absolute_path}.")
    except Exception as e:
        return ToolResult(content=f"Error writing file: {e}", is_error=True)


@registry.register(
    name="grep_search",
    description="Search for a text pattern inside a directory or file using grep logic.",
    requires_confirmation=False,
)
def grep_search(search_path: str, query: str) -> ToolResult:
    """Search for a string in files."""
    if (denied := _deny_secret(search_path)) is not None:
        return denied
    path = pathlib.Path(search_path)
    if not path.exists():
        return ToolResult(
            content=f"Error: Path {search_path} does not exist.", is_error=True
        )

    try:
        # For MVP we use builtin grep via subprocess. No regex for MVP, just exact match
        # Never descend into secret locations when searching a parent directory.
        excludes = ["--exclude-dir=personal", "--exclude-dir=.git"]
        excludes += [f"--exclude={name}" for name in sorted(PROTECTED_FILE_NAMES)]
        args = ["grep", "-rnI", "-F", *excludes, "--", query, str(path)]
        result = subprocess.run(
            args,
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
            timeout=GREP_TIMEOUT_SECONDS,
        )

        if result.returncode == 0:
            out = result.stdout
            if len(out) > 5000:
                out = out[:5000] + "\\n...[truncated because it's too long]"
            return ToolResult(content=f"Found matches:\\n{out}")
        elif result.returncode == 1:
            return ToolResult(content="No matches found.")
        else:
            return ToolResult(
                content=f"Error running search: {result.stderr}", is_error=True
            )
    except subprocess.TimeoutExpired:
        return ToolResult(
            content=f"Error: search timed out after {GREP_TIMEOUT_SECONDS}s.",
            is_error=True,
        )
    except Exception as e:
        return ToolResult(content=f"Error searching: {e}", is_error=True)
