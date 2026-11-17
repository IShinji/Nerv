"""Shared runtime policy for all agent invocations.

Cross-cutting behavioral rules live here instead of being duplicated across
every agent YAML. This keeps agent definitions focused on domain expertise.
"""

from nerv.capabilities import capability_registry
from nerv.orchestrator.registry import AgentDefinition

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
    sections = [
        BASE_RUNTIME_POLICY,
        "",
        "[Domain Instructions]",
        agent.system_prompt.strip(),
    ]

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
- **Proactive Artifact Storage**: If the user asks for a report, table, plan, or large piece of code, DO NOT dump huge blocks of text directly into the chat response. You MUST automatically use the file tools to save the deliverable to the "personal/workspace/" directory relative to the project root.
- **Dynamic File Naming**: Every generated artifact MUST include the current date in the filename for easy retrieval (e.g., "personal/workspace/2026-04-11_Model_Report.md"). Use the current_time tool if you don't know the exact date.
- **File Delivery**: Whenever you save a final deliverable for the user, you MUST include the exact string `[DOCUMENT: <filepath>]` (e.g., `[DOCUMENT: personal/workspace/2026-04-11_Model_Report.md]`) in your final chat reply to the user. The system gateway will automatically intercept this token and send the actual file directly to the user's messaging client.
- **Private Scratchpad**: Use the "personal/scratch/{agent.name.lower()}/" directory exclusively for your temporary processing data, messy notes, or intermediate screenshots.
- Never save unstructured or intermediate files to the root directory or arbitrary paths unless explicitly requested."""


def build_tool_policy(agent: AgentDefinition) -> str:
    """Generate runtime guidance derived from actually-available tools."""
    available_resolutions = [capability_registry.resolve(name) for name in agent.tools]
    available_resolutions = [
        resolution for resolution in available_resolutions if resolution
    ]
    if not available_resolutions:
        return ""

    available_labels = capability_registry.describe(agent.tools)
    available_provider_names = {
        resolution.provider_name for resolution in available_resolutions
    }
    available_requested_names = {
        resolution.requested_name for resolution in available_resolutions
    }

    def has_name(*names: str) -> bool:
        """Check whether a requested capability or provider name is available."""
        return any(
            name in available_requested_names or name in available_provider_names
            for name in names
        )

    lines = [
        "[Tool Policy]",
        f"- Available tools in this runtime: {', '.join(sorted(available_labels))}.",
        "- Prefer tools over guessing when the answer depends on current time, the filesystem, web content, or command execution.",
        "- Never claim you used a tool unless the tool was actually called and returned a result.",
    ]

    if has_name("current_time"):
        lines.append(
            "- Use current_time for questions about the current time, date, timezone, or 'what time is it'."
        )
    if has_name("file_io", "read_file", "filesystem.read", "filesystem.read_exact"):
        lines.append(
            "- Use filesystem.read / filesystem.read_exact or file_io/read_file for file inspection instead of guessing file contents."
        )
    if has_name("list_workflows", "get_workflow"):
        lines.append(
            "- Check shared workflows before inventing a new multi-step execution plan."
        )
    if has_name("execute_workflow"):
        lines.append(
            "- If a user asks for a task that closely matches a shared workflow in your library, use the execute_workflow tool. However, ALWAYS favor fast, lightweight native tools (like web_search) over heavy UI automation workflows unless the user specifically asks to use the workflow or UI tool."
        )
    if has_name("list_skills", "get_skill"):
        lines.append(
            "- Check shared skills when you need reusable operating guidance for a task domain."
        )
    if has_name("propose_workflow"):
        lines.append(
            "- When a repeated multi-step task lacks a shared workflow, draft one with propose_workflow and send it to the review queue instead of publishing it directly."
        )
    if has_name("list_review_queue"):
        lines.append(
            "- Use list_review_queue when you need to inspect pending workflow proposals."
        )
    if has_name("web_search"):
        lines.append(
            "- Use web_search for current events, public web lookup, and finding candidate pages."
        )
    if has_name("mcp_presets"):
        lines.append(
            "- Use mcp_presets to inspect builtin MCP integrations or generate a ready-to-copy config snippet for a known server."
        )
    if has_name("mcp_status"):
        lines.append(
            "- Use mcp_status when you need to inspect MCP configuration, health, or available remote tools."
        )
    if has_name("browser", "browser.read"):
        lines.append(
            "- Use browser.read / browser to inspect a specific page after you already know the URL or found it via web_search."
        )
    if has_name("browser.interactive", "browser_interactive", "chrome_browser"):
        lines.append(
            "- Use browser.interactive for interactive website tasks. Nerv should prefer a configured MCP browser provider and fall back to local chrome_browser when MCP is unavailable."
        )
    if has_name("screenshot"):
        lines.append(
            "- Use screenshot when you need the current screen state. It is privacy-sensitive and may require confirmation."
        )
    if has_name("desktop_control"):
        lines.append(
            "- Use desktop_control for explicit local app, keyboard, or URL actions. These actions require confirmation before execution."
        )
    if has_name("write_file_full"):
        lines.append(
            "- Use write_file_full only when you need to overwrite a file, and expect confirmation for destructive writes."
        )
    if has_name("shell", "local.exec"):
        lines.append(
            "- Use local.exec / shell for explicit local computer operations when file/web tools are insufficient, and expect confirmation for risky actions."
        )
    if any(
        has_name(tool_name)
        for tool_name in (
            "desktop_control",
            "screenshot",
            "write_file_full",
            "shell",
            "local.exec",
        )
    ):
        lines.append(
            "- High-risk actions may be queued as pending actions. Tell the user to reply with confirm <id> or cancel <id>."
        )

    return "\n".join(lines)
