"""Shared config loading for the Python brain layer."""

from __future__ import annotations

from pathlib import Path
from typing import Any

import yaml


def _merge_yaml(base: Any, override_value: Any) -> Any:
    """Deep-merge YAML-like structures with override precedence."""
    if isinstance(base, dict) and isinstance(override_value, dict):
        merged = dict(base)
        for key, value in override_value.items():
            if key in merged:
                merged[key] = _merge_yaml(merged[key], value)
            else:
                merged[key] = value
        return merged
    return override_value


def load_project_config(project_root: Path) -> dict[str, Any]:
    """Load default and user config with the same merge semantics as Rust."""
    default_path = project_root / "core" / "config.default.yaml"
    user_path = project_root / "config.yaml"

    merged: dict[str, Any] = {}
    if default_path.exists():
        merged = yaml.safe_load(default_path.read_text(encoding="utf-8")) or {}

    if user_path.exists():
        override_value = yaml.safe_load(user_path.read_text(encoding="utf-8")) or {}
        merged = _merge_yaml(merged, override_value)

    return merged


# ── Model selection ──────────────────────────────────────────────────────────
# Model strings use a provider-prefix scheme so the active backend is a pure
# config switch (no code change needed to swap providers):
#     "claude-cli:<alias>"  → local Claude Code CLI (uses your Claude subscription)
#     "local:<ollama-model>" → local Ollama HTTP (reserved; dormant)
#     "litellm:<model>" / bare cloud name → litellm cloud API (reserved)
DEFAULT_ROUTER_MODEL = "claude-cli:opus"
DEFAULT_TIER_MODELS: dict[int, str] = {
    0: "claude-cli:opus",
    1: "claude-cli:opus",
    2: "claude-cli:opus",
    3: "claude-cli:opus",
}


def get_models_config(project_root: Path) -> dict[str, Any]:
    """Return the merged `models` config block."""
    config = load_project_config(project_root)
    models = config.get("models", {})
    return models if isinstance(models, dict) else {}


def get_router_model(project_root: Path) -> str:
    """Resolve the router model string from config (env override wins)."""
    import os

    override = os.environ.get("NERV_ROUTER_MODEL")
    if override:
        return override
    router = get_models_config(project_root).get("router", {})
    if isinstance(router, dict):
        model = str(router.get("model", "")).strip()
        if model:
            return model
    return DEFAULT_ROUTER_MODEL


def get_tier_models(project_root: Path) -> dict[int, str]:
    """Resolve the tier→model mapping from config (env overrides win per tier)."""
    import os

    tiers_cfg = get_models_config(project_root).get("tiers", {})
    if not isinstance(tiers_cfg, dict):
        tiers_cfg = {}

    resolved: dict[int, str] = {}
    for tier in range(4):
        env_override = os.environ.get(f"NERV_MODEL_TIER{tier}")
        if env_override:
            resolved[tier] = env_override
            continue
        configured = str(tiers_cfg.get(f"tier{tier}", "")).strip()
        resolved[tier] = configured or DEFAULT_TIER_MODELS[tier]
    return resolved


def get_workspace_root(project_root: Path) -> Path:
    """Resolve the sandbox workspace root for tool write-access decisions.

    Tools may freely modify paths inside this root; operations that touch
    paths outside it are routed through the user-confirmation queue.
    """
    config = load_project_config(project_root)
    security = config.get("security", {})
    if isinstance(security, dict):
        configured = str(security.get("workspace_root", "")).strip()
        if configured:
            return Path(configured).expanduser().resolve()
    return project_root.resolve()
