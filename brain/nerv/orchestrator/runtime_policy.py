"""Shared runtime policy for all agent invocations.

Cross-cutting behavioral rules live here instead of being duplicated across
every agent YAML. This keeps agent definitions focused on domain expertise.
"""

import nerv.tools.builtins  # Ensure builtin tools are registered

from nerv.orchestrator.registry import AgentDefinition
from nerv.tools.registry import registry

BASE_RUNTIME_POLICY = """\
[Runtime Policy]
- Reply in the same language as the user's latest message unless they explicitly ask for another language.
- Never claim you cannot speak the user's language unless a real system limitation prevents it.
- Do not invent capabilities, tool results, current time, external facts, or sources.
- If you are unsure, say so plainly instead of fabricating an answer.
- Keep answers concise by default unless the user asks for more detail.
- Only rely on tools that are actually exposed to you in this runtime. If a capability is unavailable, say so directly.\
"""


def build_system_prompt(agent: AgentDefinition) -> str:
    """Compose the final system prompt for an agent invocation."""
    sections = [BASE_RUNTIME_POLICY, "", "[Domain Instructions]", agent.system_prompt.strip()]

    tool_policy = build_tool_policy(agent)
    if tool_policy:
        sections.extend(["", tool_policy])

    storage_policy = build_storage_policy(agent)
    if storage_policy:
        sections.extend(["", storage_policy])

    return "\n".join(section for section in sections if section)

def build_storage_policy(agent: AgentDefinition) -> str:
    """Generate the storage routing constraints for the Agent."""
    return f"""\
[Storage & Workspace Policy]
- All final reports, code outputs, and user-facing deliverables MUST be written to the "personal/workspace/" directory relative to the project root.
- Use the "personal/scratch/{agent.name.lower()}/" directory exclusively for your temporary processing data, messy notes, or intermediate screenshots.
- Never save unstructured or intermediate files to the root directory or arbitrary paths unless explicitly requested."""


def build_tool_policy(agent: AgentDefinition) -> str:
    """Generate runtime guidance derived from actually-available tools."""
    available_tools = [tool_name for tool_name in agent.tools if registry.get_tool(tool_name)]
    if not available_tools:
        return ""

    lines = [
        "[Tool Policy]",
        f"- Available tools in this runtime: {', '.join(sorted(available_tools))}.",
        "- Prefer tools over guessing when the answer depends on current time, the filesystem, web content, or command execution.",
        "- Never claim you used a tool unless the tool was actually called and returned a result.",
    ]

    if "current_time" in available_tools:
        lines.append("- Use current_time for questions about the current time, date, timezone, or 'what time is it'.")
    if "file_io" in available_tools or "read_file" in available_tools:
        lines.append("- Use file_io/read_file/grep_search for file inspection instead of guessing file contents.")
    if "list_workflows" in available_tools or "get_workflow" in available_tools:
        lines.append("- Check shared workflows before inventing a new multi-step execution plan.")
    if "execute_workflow" in available_tools:
        lines.append("- If a user asks for a task that closely matches a shared workflow in your library, use the execute_workflow tool. However, ALWAYS favor fast, lightweight native tools (like web_search) over heavy UI automation workflows unless the user specifically asks to use the workflow or UI tool.")
    if "list_skills" in available_tools or "get_skill" in available_tools:
        lines.append("- Check shared skills when you need reusable operating guidance for a task domain.")
    if "propose_workflow" in available_tools:
        lines.append("- When a repeated multi-step task lacks a shared workflow, draft one with propose_workflow and send it to the review queue instead of publishing it directly.")
    if "list_review_queue" in available_tools:
        lines.append("- Use list_review_queue when you need to inspect pending workflow proposals.")
    if "web_search" in available_tools:
        lines.append("- Use web_search for current events, public web lookup, and finding candidate pages.")
    if "browser" in available_tools:
        lines.append("- Use browser to inspect a specific page after you already know the URL or found it via web_search.")
    if "chrome_browser" in available_tools:
        lines.append("- Use chrome_browser for interactive website tasks inside local Google Chrome, such as opening a site, filling a prompt, submitting it, waiting for results, and extracting page text.")
    if "screenshot" in available_tools:
        lines.append("- Use screenshot when you need the current screen state. It is privacy-sensitive and may require confirmation.")
    if "desktop_control" in available_tools:
        lines.append("- Use desktop_control for explicit local app, keyboard, or URL actions. These actions require confirmation before execution.")
    if "write_file_full" in available_tools:
        lines.append("- Use write_file_full only when you need to overwrite a file, and expect confirmation for destructive writes.")
    if "shell" in available_tools:
        lines.append("- Use shell for explicit local computer operations when file/web tools are insufficient, and expect confirmation for risky actions.")
    if any(
        tool_name in available_tools
        for tool_name in ("desktop_control", "screenshot", "write_file_full", "shell")
    ):
        lines.append("- High-risk actions may be queued as pending actions. Tell the user to reply with confirm <id> or cancel <id>.")

    return "\n".join(lines)
