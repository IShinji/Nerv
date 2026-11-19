//! Shared runtime policy for all agent invocations.
//!
//! Port of `brain/nerv/orchestrator/runtime_policy.py`. Cross-cutting
//! behavioral rules live here instead of being duplicated across every agent
//! YAML, so agent definitions stay focused on domain expertise.

use super::registry::AgentDefinition;
use crate::tools::registry::ToolRegistry;

pub const BASE_RUNTIME_POLICY: &str = "\
[Runtime Policy]
- Reply in the same language as the user's latest message unless they explicitly ask for another language.
- Never claim you cannot speak the user's language unless a real system limitation prevents it.
- Do not invent capabilities, tool results, current time, external facts, or sources.
- If you are unsure, say so plainly instead of fabricating an answer.
- Keep answers concise by default unless the user asks for more detail.
- Only rely on tools that are actually exposed to you in this runtime. If a capability is unavailable, say so directly.";

/// The abstract-capability → provider alias table (port of the python
/// capability registry's `register` calls). A name resolves either to a
/// native tool or through one of these aliases.
fn capability_provider(name: &str) -> Option<&'static str> {
    match name {
        "browser.read" => Some("browser"),
        "browser.interactive" => Some("browser_interactive"),
        "filesystem.read" => Some("file_io"),
        "filesystem.read_exact" => Some("read_file"),
        "local.exec" => Some("shell"),
        "calendar.add" => Some("calendar_add"),
        "calendar.read" => Some("calendar_list"),
        "notes.write" => Some("note_write"),
        "notes.read" => Some("note_read"),
        "email.send" => Some("send_email"),
        "agent.evolve" => Some("evolve_agent"),
        "agent.versions" => Some("list_agent_versions"),
        "agent.rollback" => Some("rollback_agent"),
        _ => None,
    }
}

/// Resolve a requested name to its provider tool name, or `None` when the
/// runtime exposes nothing under that name.
pub fn resolve_provider(name: &str) -> Option<String> {
    let registry = ToolRegistry;
    if registry.get_tool(name).is_some() {
        return Some(name.to_string());
    }
    capability_provider(name).and_then(|provider| {
        if registry.get_tool(provider).is_some() {
            Some(provider.to_string())
        } else {
            None
        }
    })
}

/// Compose the final system prompt for an agent invocation.
pub fn build_system_prompt(agent: &AgentDefinition) -> String {
    // Same section layout as the python original: empty strings join as
    // blank separator lines.
    let mut sections = vec![
        BASE_RUNTIME_POLICY.to_string(),
        String::new(),
        "[Domain Instructions]".to_string(),
        agent.system_prompt.trim().to_string(),
    ];
    let tool_policy = build_tool_policy(agent);
    if !tool_policy.is_empty() {
        sections.push(String::new());
        sections.push(tool_policy);
    }
    sections.push(String::new());
    sections.push(build_storage_policy(agent));
    sections.join("\n")
}

/// The storage routing constraints (always present).
fn build_storage_policy(agent: &AgentDefinition) -> String {
    format!("\
[Storage & Workspace Policy]
- **Proactive Artifact Storage**: If the user asks for a report, table, plan, or large piece of code, DO NOT dump huge blocks of text directly into the chat response. You MUST automatically use the file tools to save the deliverable to the \"personal/workspace/\" directory relative to the project root.
- **Dynamic File Naming**: Every generated artifact MUST include the current date in the filename for easy retrieval (e.g., \"personal/workspace/2026-04-11_Model_Report.md\"). Use the current_time tool if you don't know the exact date.
- **File Delivery**: Whenever you save a final deliverable for the user, you MUST include the exact string `[DOCUMENT: <filepath>]` (e.g., `[DOCUMENT: personal/workspace/2026-04-11_Model_Report.md]`) in your final chat reply to the user. The system gateway will automatically intercept this token and send the actual file directly to the user's messaging client.
- **Private Scratchpad**: Use the \"personal/scratch/{}/\" directory exclusively for your temporary processing data, messy notes, or intermediate screenshots.
- Never save unstructured or intermediate files to the root directory or arbitrary paths unless explicitly requested.", agent.name.to_lowercase())
}

/// Runtime guidance derived from actually-available tools.
pub fn build_tool_policy(agent: &AgentDefinition) -> String {
    // (requested, provider) pairs for every available name.
    let resolved: Vec<(String, String)> = agent
        .tools
        .iter()
        .filter_map(|name| resolve_provider(name).map(|provider| (name.clone(), provider)))
        .collect();
    if resolved.is_empty() {
        return String::new();
    }

    let mut labels: Vec<&str> = resolved
        .iter()
        .map(|(requested, _)| requested.as_str())
        .collect();
    labels.sort_unstable();

    let has_name = |names: &[&str]| {
        names.iter().any(|name| {
            resolved
                .iter()
                .any(|(requested, provider)| requested == name || provider == name)
        })
    };

    let mut lines = vec![
        "[Tool Policy]".to_string(),
        format!("- Available tools in this runtime: {}.", labels.join(", ")),
        "- Prefer tools over guessing when the answer depends on current time, the filesystem, web content, or command execution.".to_string(),
        "- Never claim you used a tool unless the tool was actually called and returned a result.".to_string(),
    ];
    let mut push = |line: &str| lines.push(line.to_string());

    if has_name(&["current_time"]) {
        push("- Use current_time for questions about the current time, date, timezone, or 'what time is it'.");
    }
    if has_name(&[
        "file_io",
        "read_file",
        "filesystem.read",
        "filesystem.read_exact",
    ]) {
        push("- Use filesystem.read / filesystem.read_exact or file_io/read_file for file inspection instead of guessing file contents.");
    }
    if has_name(&["list_workflows", "get_workflow"]) {
        push("- Check shared workflows before inventing a new multi-step execution plan.");
    }
    if has_name(&["execute_workflow"]) {
        push("- If a user asks for a task that closely matches a shared workflow in your library, use the execute_workflow tool. However, ALWAYS favor fast, lightweight native tools (like web_search) over heavy UI automation workflows unless the user specifically asks to use the workflow or UI tool.");
    }
    if has_name(&["list_skills", "get_skill"]) {
        push("- Check shared skills when you need reusable operating guidance for a task domain.");
    }
    if has_name(&["propose_workflow"]) {
        push("- When a repeated multi-step task lacks a shared workflow, draft one with propose_workflow and send it to the review queue instead of publishing it directly.");
    }
    if has_name(&["list_review_queue"]) {
        push("- Use list_review_queue when you need to inspect pending workflow proposals.");
    }
    if has_name(&["web_search"]) {
        push(
            "- Use web_search for current events, public web lookup, and finding candidate pages.",
        );
    }
    if has_name(&["mcp_presets"]) {
        push("- Use mcp_presets to inspect builtin MCP integrations or generate a ready-to-copy config snippet for a known server.");
    }
    if has_name(&["mcp_status"]) {
        push("- Use mcp_status when you need to inspect MCP configuration, health, or available remote tools.");
    }
    if has_name(&["browser", "browser.read"]) {
        push("- Use browser.read / browser to inspect a specific page after you already know the URL or found it via web_search.");
    }
    if has_name(&[
        "browser.interactive",
        "browser_interactive",
        "chrome_browser",
    ]) {
        push("- Use browser.interactive for interactive website tasks. Nerv should prefer a configured MCP browser provider and fall back to local chrome_browser when MCP is unavailable.");
    }
    if has_name(&["screenshot"]) {
        push("- Use screenshot when you need the current screen state. It is privacy-sensitive and may require confirmation.");
    }
    if has_name(&["desktop_control"]) {
        push("- Use desktop_control for explicit local app, keyboard, or URL actions. These actions require confirmation before execution.");
    }
    if has_name(&["write_file_full"]) {
        push("- Use write_file_full only when you need to overwrite a file, and expect confirmation for destructive writes.");
    }
    if has_name(&["shell", "local.exec"]) {
        push("- Use local.exec / shell for explicit local computer operations when file/web tools are insufficient, and expect confirmation for risky actions.");
    }
    if has_name(&[
        "desktop_control",
        "screenshot",
        "write_file_full",
        "shell",
        "local.exec",
    ]) {
        push("- High-risk actions may be queued as pending actions. Tell the user to reply with confirm <id> or cancel <id>.");
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(tools: &[&str]) -> AgentDefinition {
        let mut a = AgentDefinition::default_named("Coder");
        a.system_prompt = "You write code.".to_string();
        a.tools = tools.iter().map(|s| s.to_string()).collect();
        a
    }

    #[test]
    fn base_policy_and_domain_instructions_are_always_present() {
        let prompt = build_system_prompt(&agent(&[]));
        assert!(prompt.contains("[Runtime Policy]"));
        assert!(prompt.contains("[Domain Instructions]"));
        assert!(prompt.contains("You write code."));
        assert!(prompt.contains("[Storage & Workspace Policy]"));
        assert!(prompt.contains("personal/scratch/coder/"));
        assert!(!prompt.contains("[Tool Policy]"));
    }

    #[test]
    fn unknown_tools_produce_no_tool_policy() {
        let prompt = build_system_prompt(&agent(&["does_not_exist"]));
        assert!(!prompt.contains("[Tool Policy]"));
    }

    #[test]
    fn capability_alias_resolves_to_provider_policy() {
        // "local.exec" is an alias for the shell tool.
        let prompt = build_system_prompt(&agent(&["local.exec"]));
        assert!(prompt.contains("[Tool Policy]"));
        assert!(prompt.contains("local.exec"));
        assert!(prompt.contains("Use local.exec / shell"));
        // High-risk follow-up line fires for shell too.
        assert!(prompt.contains("confirm <id>"));
    }

    #[test]
    fn tool_list_is_sorted_and_deduped_labels() {
        let prompt = build_system_prompt(&agent(&["web_search", "current_time"]));
        assert!(
            prompt.contains("current_time, web_search")
                || prompt.contains("current_time,web_search")
        );
        assert!(prompt.contains("Use current_time for questions"));
        assert!(prompt.contains("Use web_search for current events"));
    }
}
