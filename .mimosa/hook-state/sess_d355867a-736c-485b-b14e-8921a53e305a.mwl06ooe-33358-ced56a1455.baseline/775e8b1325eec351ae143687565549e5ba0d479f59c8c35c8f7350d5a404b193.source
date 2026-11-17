"""Builtin tools for agents.

The tools themselves live in focused modules alongside this one; importing
`nerv.tools.builtins` registers every builtin, which is the contract the rest
of Nerv (and the MCP server) depends on.

The re-exports below are the stable surface other modules import from. Note
that patching a name here does *not* affect the module that defines it —
tests monkeypatching a tool's internals must target the owning module.
"""

# Imported for their registration side effects: each module registers its
# tools on the shared `registry` at import time.
from nerv.tools import desktop as desktop  # noqa: F401
from nerv.tools import files as files  # noqa: F401
from nerv.tools import knowledge as knowledge  # noqa: F401
from nerv.tools import memory as memory  # noqa: F401
from nerv.tools import system as system  # noqa: F401
from nerv.tools import web as web  # noqa: F401
from nerv.tools._helpers import (  # noqa: F401
    MAX_TOOL_OUTPUT_CHARS,
    _get_project_root,
    _truncate,
)
from nerv.tools.desktop import (  # noqa: F401
    browser_interactive,
    chrome_browser,
    desktop_control,
    screenshot,
)
from nerv.tools.files import (  # noqa: F401
    file_io,
    grep_search,
    read_file,
    write_file_full,
)
from nerv.tools.knowledge import mcp_presets, mcp_status  # noqa: F401
from nerv.tools.memory import get_fact_section, search_memory  # noqa: F401
from nerv.tools.registry import ToolResult, registry  # noqa: F401
from nerv.tools.system import current_time, delegate_task, shell  # noqa: F401
from nerv.tools.web import browser, web_search  # noqa: F401
