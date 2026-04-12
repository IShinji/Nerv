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
