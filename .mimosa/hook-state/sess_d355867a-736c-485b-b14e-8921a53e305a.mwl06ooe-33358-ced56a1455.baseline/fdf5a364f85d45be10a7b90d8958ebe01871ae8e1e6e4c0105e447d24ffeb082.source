"""Tests for config-driven model resolution (the provider switch lives here).

The router model is resolved on the Rust side (core/brain); only the tiers —
which the python orchestrator still consumes — are tested here.
"""

from pathlib import Path

from nerv.config import get_tier_models, get_workspace_root


def _write_config(root: Path, body: str) -> None:
    root.mkdir(parents=True, exist_ok=True)
    (root / "config.yaml").write_text(body, encoding="utf-8")


def test_tier_models_read_from_config(tmp_path: Path) -> None:
    _write_config(
        tmp_path,
        """
models:
  router:
    model: "claude-cli:opus"
  tiers:
    tier0: "claude-cli:opus"
    tier1: "local:qwen2.5:1.5b"
    tier2: "litellm:claude-opus-4-8"
    tier3: "claude-cli:opus"
""",
    )
    tiers = get_tier_models(tmp_path)
    assert tiers[1] == "local:qwen2.5:1.5b"
    assert tiers[2] == "litellm:claude-opus-4-8"


def test_defaults_when_unconfigured(tmp_path: Path) -> None:
    assert get_tier_models(tmp_path)[0] == "claude-cli:opus"


def test_env_override_wins(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setenv("NERV_MODEL_TIER2", "claude-cli:sonnet")
    assert get_tier_models(tmp_path)[2] == "claude-cli:sonnet"


def test_workspace_root_defaults_to_project_root(tmp_path: Path) -> None:
    assert get_workspace_root(tmp_path) == tmp_path.resolve()


def test_workspace_root_honors_config(tmp_path: Path) -> None:
    custom = tmp_path / "ws"
    custom.mkdir()
    _write_config(tmp_path, f'security:\n  workspace_root: "{custom}"\n')
    assert get_workspace_root(tmp_path) == custom.resolve()
