"""Tests for the workspace sandbox confirmation policy."""

from pathlib import Path

from nerv.security.sandbox import confirmation_reason, is_within_sandbox


def test_paths_inside_and_outside_sandbox(tmp_path: Path) -> None:
    assert is_within_sandbox(str(tmp_path / "sub" / "file.txt"), tmp_path)
    assert is_within_sandbox(str(tmp_path), tmp_path)
    assert not is_within_sandbox("/etc/hosts", tmp_path)
    # Empty path means the tool uses its own in-workspace default.
    assert is_within_sandbox("", tmp_path)


def test_shell_and_desktop_always_need_confirmation(tmp_path: Path) -> None:
    assert confirmation_reason("shell", {"command": "ls"}, tmp_path)
    assert confirmation_reason("desktop_control", {"action": "open"}, tmp_path)


def test_file_write_gated_by_path(tmp_path: Path) -> None:
    inside = {"absolute_path": str(tmp_path / "out.txt"), "content": "x"}
    outside = {"absolute_path": "/tmp/escape.txt", "content": "x"}
    assert confirmation_reason("write_file_full", inside, tmp_path) is None
    assert confirmation_reason("write_file_full", outside, tmp_path) is not None


def test_safe_tools_never_need_confirmation(tmp_path: Path) -> None:
    assert confirmation_reason("current_time", {}, tmp_path) is None
    read_args = {"absolute_path": "/etc/hosts"}
    assert confirmation_reason("read_file", read_args, tmp_path) is None
