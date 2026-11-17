//! Tools for self-evolving agents: evolve, list versions, roll back.
//!
//! Port of `brain/nerv/evolution.py` (AgentVersionStore) and
//! `brain/nerv/tools/evolution_tools.py`. These edit agent YAML under
//! `personal/agents/`, always snapshotting the prior version first so
//! evolution is auditable and reversible.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::json;

use super::registry::{ToolDefinition, ToolResult};
use crate::llm::{ChatMessage, CompleteOptions};

const EVOLVE_SYSTEM_PROMPT: &str = "You evolve an existing AI agent definition. You are given the agent's current\nYAML and an update instruction. Return the COMPLETE updated YAML, preserving the\nstructure and all fields. Typically you revise or extend `system_prompt` to\nintegrate the new knowledge or behavior. Keep `name` unchanged. Output ONLY raw\nYAML, no markdown fences, no commentary.";

/// One archived version of an agent definition.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct VersionInfo {
    pub version: String,
    pub ts: f64,
    #[serde(default)]
    pub note: String,
}

/// Snapshot, list, and roll back agent definition versions.
pub struct AgentVersionStore {
    pub agents_dir: PathBuf,
    pub history_dir: PathBuf,
}

impl AgentVersionStore {
    pub fn new(project_root: &Path) -> Self {
        let agents_dir = project_root.join("personal").join("agents");
        Self {
            history_dir: agents_dir.join("history"),
            agents_dir,
        }
    }

    /// Locate an agent's YAML by filename stem or its `name:` field.
    pub fn find_file(&self, agent_name: &str) -> Option<PathBuf> {
        if !self.agents_dir.exists() {
            return None;
        }
        let target = agent_name.trim().to_lowercase();
        // Match by filename stem first (cheap, exact).
        let by_stem = self.agents_dir.join(format!("{target}.yaml"));
        if by_stem.exists() {
            return Some(by_stem);
        }
        // Otherwise match the `name:` field, case-insensitively.
        for path in self.agent_yaml_files() {
            for line in std::fs::read_to_string(&path).unwrap_or_default().lines() {
                let stripped = line.trim();
                if let Some(rest) = stripped.to_lowercase().strip_prefix("name:") {
                    let value = rest.trim().trim_matches(['"', '\'']).to_string();
                    if value == target {
                        return Some(path);
                    }
                    break;
                }
            }
        }
        None
    }

    fn agent_yaml_files(&self) -> Vec<PathBuf> {
        std::fs::read_dir(&self.agents_dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("yaml"))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn agent_history_dir(&self, file: &Path) -> PathBuf {
        self.history_dir.join(
            file.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
        )
    }

    fn index_path(&self, file: &Path) -> PathBuf {
        self.agent_history_dir(file).join("index.json")
    }

    fn read_index(&self, file: &Path) -> Vec<VersionInfo> {
        let path = self.index_path(file);
        if !path.exists() {
            return Vec::new();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Archive the current agent definition; return the new version id.
    pub fn snapshot(&self, agent_name: &str, note: &str) -> Option<String> {
        let file = self.find_file(agent_name)?;
        let hist = self.agent_history_dir(&file);
        let _ = std::fs::create_dir_all(&hist);
        let mut version = chrono::Local::now().format("%Y%m%dT%H%M%S").to_string();
        // Disambiguate if two snapshots land in the same second.
        let mut snap_path = hist.join(format!("{version}.yaml"));
        let mut suffix = 1;
        while snap_path.exists() {
            version = format!("{version}-{suffix}");
            snap_path = hist.join(format!("{version}.yaml"));
            suffix += 1;
        }

        std::fs::write(
            &snap_path,
            std::fs::read_to_string(&file).unwrap_or_default(),
        )
        .ok()?;
        let mut index = self.read_index(&file);
        index.push(VersionInfo {
            version: version.clone(),
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
            note: note.to_string(),
        });
        let _ = std::fs::write(
            self.index_path(&file),
            serde_json::to_string_pretty(&index).unwrap_or_default(),
        );
        Some(version)
    }

    /// Return archived versions for an agent, oldest last.
    pub fn list_versions(&self, agent_name: &str) -> Vec<VersionInfo> {
        let Some(file) = self.find_file(agent_name) else {
            return Vec::new();
        };
        self.read_index(&file)
    }

    /// Restore an archived version as the live definition.
    ///
    /// Snapshots the current state first, so a rollback is itself reversible.
    pub fn rollback(&self, agent_name: &str, version: &str) -> bool {
        let Some(file) = self.find_file(agent_name) else {
            return false;
        };
        let snap_path = self
            .agent_history_dir(&file)
            .join(format!("{version}.yaml"));
        if !snap_path.exists() {
            return false;
        }

        self.snapshot(agent_name, &format!("pre-rollback to {version}"));
        std::fs::write(
            &file,
            std::fs::read_to_string(&snap_path).unwrap_or_default(),
        )
        .is_ok()
    }
}

/// Strip markdown fences a model may have wrapped the YAML in.
fn clean_yaml(text: &str) -> String {
    let mut text = text.trim().to_string();
    if let Some(rest) = text.strip_prefix("```yaml") {
        text = rest.to_string();
    } else if let Some(rest) = text.strip_prefix("```") {
        text = rest.to_string();
    }
    if let Some(rest) = text.strip_suffix("```") {
        text = rest.to_string();
    }
    text.trim().to_string()
}

/// Validate a proposed agent YAML: parseable and carries name/description.
fn validate_agent_yaml(yaml_text: &str) -> Result<(), String> {
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct AgentCheck {
        name: String,
        description: String,
    }
    serde_yaml::from_str::<AgentCheck>(yaml_text)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub fn evolve_agent() -> ToolDefinition {
    ToolDefinition {
        name: "evolve_agent",
        description: "Update an existing agent's knowledge/behavior. Snapshots the old version first. Args: agent_name, update_instruction.",
        requires_confirmation: true,
        parameters: json!({
            "type": "object",
            "properties": {
                "agent_name": {"type": "string"},
                "update_instruction": {"type": "string"}
            },
            "required": ["agent_name", "update_instruction"]
        }),
        func: |ctx, args| {
            let project_root = ctx.project_root.clone();
            let agent_name = args
                .get("agent_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let update_instruction = args
                .get("update_instruction")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Box::pin(async move {
                evolve_agent_impl(&project_root, &agent_name, &update_instruction).await
            })
        },
    }
}

async fn evolve_agent_impl(
    project_root: &Path,
    agent_name: &str,
    update_instruction: &str,
) -> ToolResult {
    let store = AgentVersionStore::new(project_root);
    let Some(file) = store.find_file(agent_name) else {
        return ToolResult::error(format!("No agent named '{agent_name}'."));
    };

    let current_yaml = std::fs::read_to_string(&file).unwrap_or_default();
    let tier2 = nerv_shared::config::NervConfig::load(project_root)
        .map(|config| config.models.tiers.tier2)
        .unwrap_or_else(|_| "claude-cli:opus".to_string());

    let messages = vec![
        ChatMessage::system(EVOLVE_SYSTEM_PROMPT),
        ChatMessage::user(format!(
            "Current agent YAML:\n{current_yaml}\n\nUpdate instruction: {update_instruction}"
        )),
    ];
    let result = crate::llm::complete(
        &messages,
        &CompleteOptions {
            model: &tier2,
            temperature: 0.3,
            project_root: Some(project_root),
            ..Default::default()
        },
    )
    .await;
    let new_yaml = match result {
        Ok(result) => clean_yaml(&result.content),
        Err(e) => return ToolResult::error(format!("Evolution model call failed: {e}")),
    };

    // Validate before persisting — never write a broken definition.
    if let Err(exc) = validate_agent_yaml(&new_yaml) {
        return ToolResult::error(format!(
            "Proposed evolution was invalid and was discarded: {exc}"
        ));
    }

    let Some(version) = store.snapshot(
        agent_name,
        &update_instruction.chars().take(120).collect::<String>(),
    ) else {
        return ToolResult::error(format!("No agent named '{agent_name}'."));
    };
    if let Err(e) = std::fs::write(&file, new_yaml) {
        return ToolResult::error(format!("Write failed: {e}"));
    }
    ToolResult::ok(format!(
        "Evolved agent '{agent_name}'. Previous version saved as {version}. \
         Takes effect on the next brain reload."
    ))
}

pub fn list_agent_versions() -> ToolDefinition {
    ToolDefinition {
        name: "list_agent_versions",
        description: "List the saved version history for an agent. Arg: agent_name.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "agent_name": {"type": "string"}
            },
            "required": ["agent_name"]
        }),
        func: |ctx, args| {
            let agent_name = args
                .get("agent_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let store = AgentVersionStore::new(&ctx.project_root);
            Box::pin(async move {
                let versions = store.list_versions(&agent_name);
                if versions.is_empty() {
                    return ToolResult::ok(format!("No saved versions for '{agent_name}'."));
                }
                let lines: Vec<String> = versions
                    .iter()
                    .map(|v| {
                        let note = if v.note.is_empty() {
                            "(no note)"
                        } else {
                            &v.note
                        };
                        format!("- {}: {note}", v.version)
                    })
                    .collect();
                ToolResult::ok(format!(
                    "Versions for '{agent_name}':\n{}",
                    lines.join("\n")
                ))
            })
        },
    }
}

pub fn rollback_agent() -> ToolDefinition {
    ToolDefinition {
        name: "rollback_agent",
        description: "Roll an agent back to a saved version. Args: agent_name, version.",
        requires_confirmation: true,
        parameters: json!({
            "type": "object",
            "properties": {
                "agent_name": {"type": "string"},
                "version": {"type": "string"}
            },
            "required": ["agent_name", "version"]
        }),
        func: |ctx, args| {
            let agent_name = args
                .get("agent_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let version = args
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let store = AgentVersionStore::new(&ctx.project_root);
            Box::pin(async move {
                if store.rollback(&agent_name, &version) {
                    ToolResult::ok(format!(
                        "Rolled '{agent_name}' back to {version}. Takes effect on next reload."
                    ))
                } else {
                    ToolResult::error(format!(
                        "Could not roll back '{agent_name}' to version {version}."
                    ))
                }
            })
        },
    }
}
