"""Shared pytest fixtures."""

from __future__ import annotations

import pytest


@pytest.fixture(autouse=True)
def _isolate_user_config(
    tmp_path_factory: pytest.TempPathFactory, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Keep the developer's own Nerv config out of every test.

    ``load_project_config`` honors ``NERV_CONFIG`` and ``~/.nerv/config.yaml`` like
    the Rust loader does, so a config on the machine running the tests would
    otherwise leak into tests that expect "no user config".
    """
    monkeypatch.delenv("NERV_CONFIG", raising=False)
    monkeypatch.setenv("HOME", str(tmp_path_factory.mktemp("home")))
