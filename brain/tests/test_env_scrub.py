"""Credential-shaped environment variables must not reach shell children."""

import asyncio
from pathlib import Path

import pytest

from nerv.security.env import scrubbed_env
from nerv.tools.system import shell


def test_scrubbed_env_drops_credential_names() -> None:
    env = {
        "FOO_API_KEY": "a",
        "GH_TOKEN": "b",
        "db_password": "c",
        "MY_SECRET": "d",
        "PATH": "/usr/bin",
        "HOME": "/home/x",
        "LANG": "C",
    }
    assert scrubbed_env(env) == {"PATH": "/usr/bin", "HOME": "/home/x", "LANG": "C"}


def test_scrubbed_env_passthrough_is_exact_and_case_sensitive() -> None:
    env = {"GITHUB_TOKEN": "t", "other_token": "u"}
    out = scrubbed_env(env, passthrough=["GITHUB_TOKEN", "OTHER_TOKEN"])
    assert out == {"GITHUB_TOKEN": "t"}


def test_scrubbed_env_does_not_mutate_input() -> None:
    env = {"A_SECRET": "x", "B": "y"}
    scrubbed_env(env)
    assert env == {"A_SECRET": "x", "B": "y"}


@pytest.fixture
def isolated_root(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("NERV_PROJECT_ROOT", str(tmp_path))
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv("NERV_CONFIG", raising=False)
    return tmp_path


def test_shell_hides_secret_env(
    isolated_root: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("NERV_TEST_API_KEY", "sekrit")
    result = asyncio.run(shell("env"))
    assert "sekrit" not in result.content


def test_shell_passthrough_from_config(
    isolated_root: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("NERV_TEST_API_KEY", "sekrit")
    (isolated_root / "config.yaml").write_text(
        "security:\n  shell_env_passthrough: [NERV_TEST_API_KEY]\n"
    )
    result = asyncio.run(shell("printenv NERV_TEST_API_KEY"))
    assert "sekrit" in result.content


def test_shell_hides_vars_loaded_from_api_keys_env(
    isolated_root: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from dotenv import load_dotenv

    personal = isolated_root / "personal"
    personal.mkdir()
    keys = personal / "api_keys.env"
    keys.write_text("NERV_TEST_FAKE_PROVIDER_KEY=sekrit\n")
    monkeypatch.delenv("NERV_TEST_FAKE_PROVIDER_KEY", raising=False)
    load_dotenv(keys)
    try:
        result = asyncio.run(shell("env"))
    finally:
        monkeypatch.delenv("NERV_TEST_FAKE_PROVIDER_KEY", raising=False)
    assert "sekrit" not in result.content
