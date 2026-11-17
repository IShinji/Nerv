"""MCP request timeout is configurable per server and fails with a clear error."""

import sys
from pathlib import Path

import pytest

from nerv.mcp.client import McpServerConfig, McpTransportError, StdioMcpClient
from nerv.mcp.presets import merge_preset_into_server_config

# A server that reads requests but never answers.
_SILENT_SERVER = "import sys\nfor _ in sys.stdin:\n    pass\n"


def test_default_timeout_is_30_seconds() -> None:
    assert McpServerConfig(name="x").request_timeout == 30.0
    assert merge_preset_into_server_config("x", {}).request_timeout == 30.0


def test_timeout_read_from_config() -> None:
    cfg = merge_preset_into_server_config("x", {"request_timeout": 75})
    assert cfg.request_timeout == 75.0


def test_invalid_timeout_falls_back_to_default() -> None:
    for bad in (0, -5, "soon", None):
        cfg = merge_preset_into_server_config("x", {"request_timeout": bad})
        assert cfg.request_timeout == 30.0


async def test_timeout_error_names_server_method_and_limit(tmp_path: Path) -> None:
    cfg = McpServerConfig(
        name="slowpoke",
        enabled=True,
        command=sys.executable,
        args=["-c", _SILENT_SERVER],
        request_timeout=0.3,
    )
    client = StdioMcpClient(cfg, tmp_path)
    try:
        with pytest.raises(McpTransportError) as err:
            await client.connect()
    finally:
        await client.close()
    message = str(err.value)
    assert "slowpoke" in message and "0.3" in message
    assert "request_timeout" in message
