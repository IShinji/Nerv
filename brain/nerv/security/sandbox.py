"""Workspace sandbox policy for tool execution.

Tools may freely modify paths inside the workspace root. Operations that touch
paths outside it — or that cannot be statically scoped to a path (shell,
desktop control) — are routed through the user-confirmation queue instead of
running immediately.
"""

from __future__ import annotations

from pathlib import Path

# Tools whose effects cannot be confined to a path (or that act outward) are
# always confirmed.
ALWAYS_CONFIRM_TOOLS: frozenset[str] = frozenset(
    {"shell", "desktop_control", "send_email"}
)

# Tools that take a target path: confirmed only when the path escapes the sandbox.
PATH_ARG_BY_TOOL: dict[str, str] = {
    "write_file_full": "absolute_path",
    "screenshot": "output_path",
}


def is_within_sandbox(path: str, root: Path) -> bool:
    """Return whether ``path`` resolves to a location inside ``root``."""
    if not path:
        # An empty path means the tool uses its own in-workspace default.
        return True
    try:
        resolved = Path(path).expanduser().resolve()
    except (OSError, RuntimeError, ValueError):
        return False
    root = root.resolve()
    return resolved == root or root in resolved.parents


def confirmation_reason(
    tool_name: str,
    arguments: dict,
    workspace_root: Path,
) -> str | None:
    """Return a human reason if the call needs confirmation, else ``None``."""
    if tool_name in ALWAYS_CONFIRM_TOOLS:
        return f"'{tool_name}' can affect resources outside the workspace sandbox"

    arg_name = PATH_ARG_BY_TOOL.get(tool_name)
    if arg_name:
        target = str(arguments.get(arg_name, "")).strip()
        if not is_within_sandbox(target, workspace_root):
            return f"target path '{target}' is outside the workspace sandbox"

    return None
