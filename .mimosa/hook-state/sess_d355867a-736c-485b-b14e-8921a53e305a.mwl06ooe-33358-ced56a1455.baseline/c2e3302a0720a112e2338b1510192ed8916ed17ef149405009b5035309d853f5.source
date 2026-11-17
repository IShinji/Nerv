"""Entry point so `python -m nerv.mcp_server` launches the stdio tool server."""

import asyncio
import logging
import sys

from nerv.mcp_server.server import run

logging.basicConfig(
    level=logging.INFO,
    format="[nerv-mcp] %(asctime)s %(levelname)s %(name)s: %(message)s",
    stream=sys.stderr,
)


def main() -> None:
    """Run the MCP server until the transport closes."""
    try:
        asyncio.run(run())
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
