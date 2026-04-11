"""Builtin Tools for agents."""

import pathlib
import subprocess

from nerv.tools.registry import ToolResult, registry

# -----------------------------------------------------------------------------
# File System Tools
# -----------------------------------------------------------------------------

@registry.register(
    name="read_file",
    description="Read the exact contents of a file at a given absolute path.",
    requires_confirmation=False,
)
def read_file(absolute_path: str) -> ToolResult:
    """Read a file."""
    path = pathlib.Path(absolute_path)
    if not path.exists():
        return ToolResult(content=f"Error: File {absolute_path} does not exist.", is_error=True)
    if not path.is_file():
        return ToolResult(content=f"Error: {absolute_path} is not a file.", is_error=True)

    try:
        content = path.read_text(encoding="utf-8")
        return ToolResult(content=content)
    except Exception as e:
        return ToolResult(content=f"Error reading file: {e}", is_error=True)


@registry.register(
    name="write_file_full",
    description="Overwrite a file entirely with new content. Use carefully as this deletes old content.",
    requires_confirmation=True,
)
def write_file_full(absolute_path: str, content: str) -> ToolResult:
    """Overwrite a file entirely."""
    path = pathlib.Path(absolute_path)
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        return ToolResult(content=f"Successfully completely wrote to {absolute_path}.")
    except Exception as e:
        return ToolResult(content=f"Error writing file: {e}", is_error=True)


@registry.register(
    name="grep_search",
    description="Search for a text pattern inside a directory or file using grep logic.",
    requires_confirmation=False,
)
def grep_search(search_path: str, query: str) -> ToolResult:
    """Search for a string in files."""
    path = pathlib.Path(search_path)
    if not path.exists():
        return ToolResult(content=f"Error: Path {search_path} does not exist.", is_error=True)

    try:
        # For MVP we use builtin grep via subprocess. No regex for MVP, just exact match
        args = ["grep", "-rnI", query, str(path)]
        result = subprocess.run(args, capture_output=True, text=True)
        
        if result.returncode == 0:
            out = result.stdout
            if len(out) > 5000:
                out = out[:5000] + "\\n...[truncated because it's too long]"
            return ToolResult(content=f"Found matches:\\n{out}")
        elif result.returncode == 1:
            return ToolResult(content="No matches found.")
        else:
            return ToolResult(content=f"Error running search: {result.stderr}", is_error=True)
    except Exception as e:
        return ToolResult(content=f"Error searching: {e}", is_error=True)

# -----------------------------------------------------------------------------
# System Tools
# -----------------------------------------------------------------------------

@registry.register(
    name="shell",
    description="Execute a shell command on the host OS. This includes running build scripts, python commands, git, etc.",
    requires_confirmation=True,  # Shell is always dangerous
)
def shell(command: str) -> ToolResult:
    """Run a shell command."""
    try:
        result = subprocess.run(
            command,
            shell=True,
            capture_output=True,
            text=True,
        )
        output = result.stdout
        if result.stderr:
            output += f"\\n[stderr]:\\n{result.stderr}"

        # Truncate to limit LLM context bloat
        if len(output) > 5000:
             output = output[:5000] + "\\n...[truncated output]"

        return ToolResult(
            content=f"Command executed. Exit code: {result.returncode}\\nOutput:\\n{output}",
            is_error=result.returncode != 0
        )
    except Exception as e:
         return ToolResult(content=f"Shell execution failed: {e}", is_error=True)

# -----------------------------------------------------------------------------
# Swarm Tools (Orchestration context needed)
# -----------------------------------------------------------------------------

@registry.register(
    name="delegate_task",
    description="Ask another specialized professional agent to do a task and return the result to you. Use this to consult domain experts.",
    requires_confirmation=False,
)
async def delegate_task(role_name: str, task_description: str) -> ToolResult:
    """Spawn or consult a sub-agent for a specific task."""
    from nerv.orchestrator.orchestrator import Orchestrator
    import pathlib
    
    # We build a temporary orchestrator for the sub-agent
    orch = Orchestrator(pathlib.Path.cwd())
    
    # Fake the route result
    from nerv.models import RouteResult
    route = RouteResult(
        intent=role_name,
        complexity="high",
        model_tier=1,
        agent_type=role_name, # This forces the Factory to spawn this exact role!
        reply=""
    )
    
    # We prefix the message so the sub-agent knows its context
    prompt = f"[DELEGATION TASK]\nYou have been spawned by a higher-level architect agent to handle a sub-task.\nTask Description: {task_description}"
    
    response = await orch.dispatch(prompt, route)
    
    return ToolResult(content=f"Expert ({role_name}) replied: {response}")

