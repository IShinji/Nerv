"""The brain must read the same user config file as the Rust gateway.

Mirrors ``user_config_path_precedence`` in ``core/shared/src/config.rs``: the file
named by ``NERV_CONFIG``, then ``<project root>/config.yaml``, then
``~/.nerv/config.yaml``. Without this, settings kept in either of the last two
places are used by the gateway but silently ignored by the brain.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from nerv.config import get_max_tool_iterations, get_workspace_root, load_project_config


def _write(path: Path, body: str = "") -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")
    return path


def _settings(workspace: Path, iterations: int) -> str:
    return (
        f'security:\n  workspace_root: "{workspace}"\n'
        f"orchestrator:\n  max_tool_iterations: {iterations}\n"
    )


def test_user_config_path_precedence(tmp_path: Path) -> None:
    from nerv.config import resolve_user_config_path

    root = tmp_path / "root"
    root.mkdir()
    home = tmp_path / "home"
    home_cfg = _write(home / ".nerv" / "config.yaml")
    env = {"HOME": str(home)}

    # Only the home fallback exists.
    assert resolve_user_config_path(root, env) == home_cfg
    # A project-root config wins over the home one.
    _write(root / "config.yaml")
    assert resolve_user_config_path(root, env) == root / "config.yaml"
    # NERV_CONFIG wins over both, even if the file is missing (a typo must be loud).
    custom = root / "nope.yaml"
    with_override = {**env, "NERV_CONFIG": str(custom)}
    assert resolve_user_config_path(root, with_override) == custom
    # An empty NERV_CONFIG counts as unset.
    assert resolve_user_config_path(root, {**env, "NERV_CONFIG": ""}) == (
        root / "config.yaml"
    )
    # Nothing anywhere, and no HOME to fall back on.
    empty = tmp_path / "empty"
    empty.mkdir()
    assert resolve_user_config_path(empty, {}) is None


def test_config_in_home_dir_is_honored(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root, home, workspace = tmp_path / "root", tmp_path / "home", tmp_path / "ws"
    root.mkdir()
    workspace.mkdir()
    _write(home / ".nerv" / "config.yaml", _settings(workspace, 3))
    monkeypatch.setenv("HOME", str(home))

    assert get_workspace_root(root) == workspace.resolve()
    assert get_max_tool_iterations(root) == 3


def test_nerv_config_env_is_honored(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root, workspace = tmp_path / "root", tmp_path / "ws"
    root.mkdir()
    workspace.mkdir()
    cfg = _write(tmp_path / "elsewhere" / "nerv.yaml", _settings(workspace, 4))
    monkeypatch.setenv("NERV_CONFIG", str(cfg))

    assert get_workspace_root(root) == workspace.resolve()
    assert get_max_tool_iterations(root) == 4


def test_nerv_config_replaces_the_project_config(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # One user file is read, not a merge of several (same as the Rust loader).
    root, workspace = tmp_path / "root", tmp_path / "ws"
    root.mkdir()
    workspace.mkdir()
    _write(root / "config.yaml", _settings(workspace, 5))
    cfg = _write(tmp_path / "nerv.yaml", "orchestrator:\n  max_tool_iterations: 4\n")
    monkeypatch.setenv("NERV_CONFIG", str(cfg))

    assert get_max_tool_iterations(root) == 4
    assert get_workspace_root(root) == root.resolve()


def test_missing_nerv_config_fails_loudly(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # The gateway refuses to start on this; falling back to defaults here would
    # silently loosen settings such as security.workspace_root.
    monkeypatch.setenv("NERV_CONFIG", str(tmp_path / "typo.yaml"))
    with pytest.raises(FileNotFoundError):
        load_project_config(tmp_path)


def _mcp_env(root: Path) -> dict[str, str]:
    from nerv.llm.claude_cli import _write_mcp_config

    path = _write_mcp_config(root)
    try:
        return json.loads(path.read_text())["mcpServers"]["nerv"]["env"]
    finally:
        path.unlink()


def test_mcp_server_gets_the_resolved_user_config(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # The MCP server is a separate process with an explicit environment: it must
    # be handed the file this process resolved, or its sandbox settings
    # (security.workspace_root) could differ from the orchestrator's.
    cfg = _write(tmp_path / "elsewhere" / "nerv.yaml")
    monkeypatch.setenv("NERV_CONFIG", str(cfg))

    assert _mcp_env(tmp_path)["NERV_CONFIG"] == str(cfg.resolve())


def test_mcp_server_gets_the_project_config(tmp_path: Path) -> None:
    cfg = _write(tmp_path / "config.yaml")

    assert _mcp_env(tmp_path)["NERV_CONFIG"] == str(cfg.resolve())


def test_mcp_env_has_no_user_config_when_there_is_none(tmp_path: Path) -> None:
    assert "NERV_CONFIG" not in _mcp_env(tmp_path)
