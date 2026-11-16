"""Schema versioning shared by Nerv's JSON state files.

Rules: a missing ``schema_version`` means 1 (files written before versioning);
writers always stamp the current version; a file with a *higher* version was
written by a newer Nerv, so older code treats it as empty and must not
overwrite it.
"""

from __future__ import annotations

import logging
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)

CURRENT_SCHEMA_VERSION = 1


def schema_version_of(data: Any) -> int:
    """Return the version recorded in a parsed state file (default 1)."""
    if isinstance(data, dict):
        value = data.get("schema_version", 1)
        if isinstance(value, int) and not isinstance(value, bool):
            return value
    return 1


def is_newer_schema(data: Any, path: Path | None = None) -> bool:
    """Whether ``data`` was written by a newer version; logs an error if so."""
    version = schema_version_of(data)
    if version <= CURRENT_SCHEMA_VERSION:
        return False
    logger.error(
        "%s has schema_version %d but this build supports up to %d; "
        "ignoring it and leaving it untouched",
        path if path is not None else "state file",
        version,
        CURRENT_SCHEMA_VERSION,
    )
    return True
