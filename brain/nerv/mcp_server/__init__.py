"""Nerv tool MCP server.

Exposes Nerv's native tools to the Claude Code CLI over stdio so the claude-cli
backend can act through them. High-risk / out-of-sandbox calls are gated here
via the shared pending-action store instead of executing immediately.
"""

from nerv.mcp_server.server import build_server, run

__all__ = ["build_server", "run"]
