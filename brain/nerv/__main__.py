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
        from nerv.config import get_router_model, get_tier_models
        from nerv.ipc import _get_project_root
        from nerv.llm import parse_model

        project_root = _get_project_root()

        # Only warm up a local Ollama model when a `local:` provider is actually
        # configured. With the claude-cli / litellm backends this is a no-op, so
        # startup never blocks pulling models we won't use.
        configured = [
            get_router_model(project_root),
            *get_tier_models(project_root).values(),
        ]
        local_models = {
            name
            for provider, name in map(parse_model, configured)
            if provider == "local"
        }
        if local_models:
            from nerv.hardware import check_and_pull_model

            for model_name in local_models:
                check_and_pull_model(model_name)

        asyncio.run(run_jsonrpc_loop())
    except KeyboardInterrupt:
        logger.info("Brain process interrupted")
    except Exception:
        logger.exception("Brain process crashed")
        sys.exit(1)


if __name__ == "__main__":
    main()
