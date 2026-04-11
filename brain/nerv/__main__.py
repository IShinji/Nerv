"""Nerv Brain entry point — JSON-RPC server over stdin/stdout.

Launched by the Rust gateway as a subprocess. Reads JSON-RPC requests
from stdin (one per line), processes them, and writes responses to stdout.
Logging goes to stderr to avoid interfering with the IPC channel.
"""

import asyncio
import logging
import sys

from nerv.ipc import run_jsonrpc_loop

# All logging goes to stderr — stdout is reserved for JSON-RPC IPC
logging.basicConfig(
    level=logging.INFO,
    format="[brain] %(asctime)s %(levelname)s %(name)s: %(message)s",
    stream=sys.stderr,
)

logger = logging.getLogger("nerv")


def main() -> None:
    """Entry point for the brain process."""
    logger.info("Nerv brain starting...")
    try:
        asyncio.run(run_jsonrpc_loop())
    except KeyboardInterrupt:
        logger.info("Brain process interrupted")
    except Exception:
        logger.exception("Brain process crashed")
        sys.exit(1)


if __name__ == "__main__":
    main()
