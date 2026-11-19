//! Agent Factory — dynamically creating missing agents (PRD v2 E9).
//!
//! Port of `brain/nerv/orchestrator/factory.py`. When a route names an agent
//! that no YAML defines, the factory asks the model to design one, validates
//! the YAML and saves it to `personal/agents/` for reuse.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{error, info};

use super::registry::AgentDefinition;
use crate::llm::{self, ChatMessage, CompleteOptions};

pub const FACTORY_TIMEOUT: Duration = Duration::from_secs(120);

pub const FACTORY_SYSTEM_PROMPT: &str = "\
You are an expert AI persona designer and software architect.
Your ONLY job is to generate a comprehensive YAML configuration file for a new AI Agent.
The YAML must EXACTLY match the following structure, with NO markdown block formatting outside of the yaml content itself (though you should just output raw yaml).

Required YAML Structure:
name: <PascalCase version of the role, e.g., FrontendDeveloper>
description: <A one sentence description of their expertise>
system_prompt: |
  <A highly detailed system prompt outlining their persona, constraints, and instructions.
  Make sure to tell the agent exactly what tools they should use and how to behave.>
tools:
  - <tool_name_1_if_applicable>
  - <tool_name_2_if_applicable>

Available capabilities and built-in tools for them to use (only include if strictly necessary for their role):
- browser.read (preferred abstract capability for inspecting known webpages; currently backed by browser)
- browser.interactive (preferred abstract capability for interactive browser tasks; routed through MCP when configured and otherwise falls back to local chrome_browser)
- filesystem.read (preferred abstract capability for directory and file inspection; currently backed by file_io)
- filesystem.read_exact (preferred abstract capability for exact file reads; currently backed by read_file)
- local.exec (preferred abstract capability for local command execution; currently backed by shell)
- current_time
- calendar_add (add an event to the local calendar)
- calendar_list (list upcoming local calendar events)
- note_write (save a Markdown note)
- note_read (read a saved note)
- note_list (list saved notes)
- send_email (send email; requires a configured email MCP server)
- file_io
- read_file
- write_file_full
- list_workflows
- get_workflow
- propose_workflow
- list_skills
- get_skill
- list_review_queue
- web_search
- browser
- mcp_presets
- mcp_status
- chrome_browser
- screenshot
- desktop_control
- shell (HIGH RISK: use only for sysadmins, fullstack devs, or testing. Can execute any bash command.)
- grep_search
- delegate_task (Allows delegating to other specialized agents. Best for architects / project managers.)

CRITICAL INSTRUCTIONS:
1. Do not wrap the output in ```yaml ... ``` code blocks. Output ONLY raw YAML text.
2. The tools must be an array of strings. Leave empty [] if none are needed.
3. Keep the system_prompt focused on their specific domain.
4. Do not include global runtime rules like language-following or generic honesty boilerplate; those are injected separately at runtime.";

/// A filesystem-safe role name, or `None` if it cannot be made safe.
///
/// Spaces become underscores; anything else outside `[A-Za-z0-9_-]` (path
/// separators, dots, traversal) is rejected rather than silently rewritten.
pub fn normalize_role_name(role_name: &str) -> Option<String> {
    let candidate = role_name.trim().replace(char::is_whitespace, "_");
    let valid = !candidate.is_empty()
        && candidate
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && candidate.len() <= 64;
    valid.then_some(candidate)
}

/// Dynamically generates YAML configurations for unknown agent roles.
pub struct AgentFactory {
    project_root: PathBuf,
    agents_dir: PathBuf,
}

impl AgentFactory {
    pub fn new(project_root: &Path) -> Self {
        let agents_dir = project_root.join("personal").join("agents");
        let _ = std::fs::create_dir_all(&agents_dir);
        Self {
            project_root: project_root.to_path_buf(),
            agents_dir,
        }
    }

    /// The tier-2 model (persona design benefits from a capable model).
    fn model(&self) -> String {
        nerv_shared::config::NervConfig::load(&self.project_root)
            .map(|c| c.models.tiers.tier2)
            .unwrap_or_else(|_| "claude-cli:opus".to_string())
    }

    /// Ask the model to design an agent for `role_name`, validate and save it.
    /// Returns the parsed definition, or `None` on any failure (the caller
    /// falls back to the general agent).
    pub async fn create_agent(
        &self,
        role_name: &str,
        user_intent_hint: &str,
    ) -> Option<AgentDefinition> {
        let Some(safe_name) = normalize_role_name(role_name) else {
            error!("Factory refused unsafe role name: {role_name:?}");
            return None;
        };
        info!("Factory is creating a new agent for role: '{safe_name}'");

        let user_prompt = format!(
            "Please generate the agent YAML for the role: '{role_name}'.\n\
             The user's original request that triggered this was: '{user_intent_hint}'.\n\
             Make the system prompt perfectly suited to handle this type of request and future similar requests."
        );

        let result = llm::complete(
            &[
                ChatMessage::new("system", FACTORY_SYSTEM_PROMPT),
                ChatMessage::new("user", user_prompt),
            ],
            &CompleteOptions {
                model: &self.model(),
                // Slightly creative for persona design.
                temperature: 0.4,
                project_root: Some(&self.project_root),
                timeout: FACTORY_TIMEOUT,
                ..Default::default()
            },
        )
        .await;

        let yaml_content = match result {
            Ok(r) => clean_yaml(&r.content),
            Err(e) => {
                error!("Factory failed to create agent '{role_name}': {e}");
                return None;
            }
        };

        // Save it, then validate by parsing through the registry's schema.
        let file_path = self.agents_dir.join(format!("{safe_name}.yaml"));
        if let Err(e) = std::fs::write(&file_path, &yaml_content) {
            error!("Factory could not save {file_path:?}: {e}");
            return None;
        }

        match serde_yaml::from_str::<AgentDefinition>(&yaml_content) {
            Ok(agent) => Some(agent),
            Err(e) => {
                error!("Factory produced invalid agent YAML for '{role_name}': {e}");
                None
            }
        }
    }
}

/// Strip markdown code-block fences if the model hallucinated them.
fn clean_yaml(text: &str) -> String {
    let mut text = text.trim();
    if let Some(rest) = text.strip_prefix("```yaml") {
        text = rest;
    } else if let Some(rest) = text.strip_prefix("```") {
        text = rest;
    }
    if let Some(rest) = text.strip_suffix("```") {
        text = rest;
    }
    text.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_names_are_sanitized_or_rejected() {
        assert_eq!(
            normalize_role_name("Frontend Developer").as_deref(),
            Some("Frontend_Developer")
        );
        assert_eq!(normalize_role_name("writer").as_deref(), Some("writer"));
        assert_eq!(normalize_role_name("../etc/passwd"), None);
        assert_eq!(normalize_role_name("data 分析"), None);
        assert_eq!(normalize_role_name(""), None);
        assert_eq!(normalize_role_name("9lives"), None);
    }

    #[test]
    fn yaml_fences_are_stripped() {
        assert_eq!(clean_yaml("```yaml\nname: X\n```"), "name: X");
        assert_eq!(clean_yaml("```\nname: X\n```"), "name: X");
        assert_eq!(clean_yaml("name: X"), "name: X");
    }

    #[test]
    fn factory_rejects_unsafe_roles_before_calling_a_model() {
        let dir = tempfile::tempdir().unwrap();
        let factory = AgentFactory::new(dir.path());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let out = runtime.block_on(factory.create_agent("../evil", "hint"));
        assert!(out.is_none());
        // Nothing was written.
        assert!(dir
            .path()
            .join("personal/agents")
            .read_dir()
            .map(|mut d| d.next().is_none())
            .unwrap_or(true));
    }

}
