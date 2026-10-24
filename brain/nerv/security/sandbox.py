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


# Paths under the project root that hold secrets, config, or executable code.
# Even inside the workspace they are never readable or writable by tools without
# confirmation (writes) or at all (reads): writing code/config is privilege
# escalation, and personal/ holds credentials.
PROTECTED_RELATIVE_PATHS: tuple[str, ...] = (
    "config.yaml",
    "personal",
    "brain",
    "core",
    "gui",
    "agents",
    "scripts",
    ".git",
    ".github",
)

# Individual file names that are protected wherever they appear.
PROTECTED_FILE_NAMES: frozenset[str] = frozenset(
    {"config.yaml", "api_keys.env", "pending_actions.json", ".env"}
)


def is_protected_path(path: str, project_root: Path) -> bool:
    """Return whether ``path`` is a secret/config/code location of the project."""
    if not path:
        return False
    try:
        resolved = Path(path).expanduser().resolve()
    except (OSError, RuntimeError, ValueError):
        return True
    if resolved.name in PROTECTED_FILE_NAMES:
        return True
    nerv_home = Path.home() / ".nerv"
    if resolved == nerv_home or nerv_home in resolved.parents:
        return True
    root = project_root.resolve()
    for rel in PROTECTED_RELATIVE_PATHS:
        guarded = root / rel
        if resolved == guarded or guarded in resolved.parents:
            return True
    return False


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
    requires_confirmation: bool = False,
    project_root: Path | None = None,
) -> str | None:
    """Return a human reason if the call needs confirmation, else ``None``.

    ``requires_confirmation`` is the tool's own registry flag; a tool that sets
    it is always confirmed (default-deny for anything not explicitly safe).
    """
    if tool_name in ALWAYS_CONFIRM_TOOLS or requires_confirmation:
        return f"'{tool_name}' can affect resources outside the workspace sandbox"

    arg_name = PATH_ARG_BY_TOOL.get(tool_name)
    if arg_name:
        target = str(arguments.get(arg_name, "")).strip()
        if not is_within_sandbox(target, workspace_root):
            return f"target path '{target}' is outside the workspace sandbox"
        if is_protected_path(target, project_root or workspace_root):
            return f"target path '{target}' is a protected config/secret/code location"

    return None
