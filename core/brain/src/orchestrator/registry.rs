//! Agent registry — loads and matches agent definitions from YAML files.
//!
//! Port of `brain/nerv/orchestrator/registry.py`. Searches two locations
//! (priority order, later wins):
//! 1. `agents/`           — preset agents shipped with the system
//! 2. `personal/agents/`  — user-created and auto-created agents

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{error, info};

/// Parsed agent definition from a YAML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_model_tier")]
    pub model_tier: Vec<i32>,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: u32,
    #[serde(default = "default_summary_strategy")]
    pub summary_strategy: String,
    #[serde(default = "default_created_by")]
    pub created_by: String,
    #[serde(skip)]
    pub source_path: PathBuf,
}

fn default_model_tier() -> Vec<i32> {
    vec![0]
}
fn default_max_context_tokens() -> u32 {
    4000
}
fn default_summary_strategy() -> String {
    "rolling".to_string()
}
fn default_created_by() -> String {
    "preset".to_string()
}

/// Registry that loads and matches agents from YAML definitions.
pub struct AgentRegistry {
    agents: HashMap<String, AgentDefinition>,
}

impl AgentRegistry {
    /// Load agent definitions from disk (preset first, then personal —
    /// personal overrides preset on a name clash).
    pub fn load(project_root: &Path) -> Self {
        let mut agents = HashMap::new();
        let preset_dir = project_root.join("agents");
        let personal_dir = project_root.join("personal").join("agents");
        for dir in [preset_dir, personal_dir] {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                    continue;
                }
                match Self::parse_agent_file(&path) {
                    Ok(agent) => {
                        agents.insert(agent.name.to_lowercase(), agent);
                    }
                    Err(e) => error!("Failed to load agent from {}: {e}", path.display()),
                }
            }
        }
        info!("Agent registry loaded: {} agents", agents.len());
        Self { agents }
    }

    /// Parse a single YAML agent definition file.
    fn parse_agent_file(path: &Path) -> Result<AgentDefinition, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut agent: AgentDefinition = serde_yaml::from_str(&text).map_err(|e| e.to_string())?;
        agent.source_path = path.to_path_buf();
        Ok(agent)
    }

    /// Find an agent by exact type name match.
    pub fn find_by_type(&self, agent_type: &str) -> Option<&AgentDefinition> {
        self.agents.get(&agent_type.to_lowercase())
    }

    /// Find the best matching agent by tag overlap.
    pub fn find_by_tags(&self, tags: &[String]) -> Option<&AgentDefinition> {
        if tags.is_empty() {
            return None;
        }
        let wanted: Vec<String> = tags.iter().map(|t| t.to_lowercase()).collect();
        self.agents
            .values()
            .max_by_key(|agent| {
                agent
                    .tags
                    .iter()
                    .filter(|t| wanted.contains(&t.to_lowercase()))
                    .count()
            })
            .filter(|best| {
                !best.tags.is_empty()
                    && best.tags.iter().any(|t| wanted.contains(&t.to_lowercase()))
            })
    }

    /// Return the general/fallback agent, or a minimal one if none exists.
    pub fn get_default(&self) -> AgentDefinition {
        self.agents.get("general").cloned().unwrap_or_else(|| {
            let mut minimal = AgentDefinition::default_named("General");
            minimal.description = "Fallback general assistant".to_string();
            minimal.system_prompt = "You are a helpful assistant. Answer concisely.".to_string();
            minimal
        })
    }

    /// All loaded agents.
    pub fn list_agents(&self) -> Vec<&AgentDefinition> {
        self.agents.values().collect()
    }

    /// Hot-reload one agent (used by the factory after creating one).
    pub fn insert(&mut self, agent: AgentDefinition) {
        self.agents.insert(agent.name.to_lowercase(), agent);
    }
}

impl AgentDefinition {
    pub(crate) fn default_named(name: &str) -> Self {
        Self {
            name: name.to_string(),
            description: String::new(),
            tags: Vec::new(),
            model_tier: default_model_tier(),
            system_prompt: String::new(),
            tools: Vec::new(),
            max_context_tokens: default_max_context_tokens(),
            summary_strategy: default_summary_strategy(),
            created_by: default_created_by(),
            source_path: PathBuf::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agent_yaml_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let agents_dir = dir.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("coder.yaml"),
            "name: Coder\nsystem_prompt: |\n  You write code.\ntools:\n  - web_search\n",
        )
        .unwrap();

        let registry = AgentRegistry::load(dir.path());
        let agent = registry.find_by_type("coder").expect("loaded by type");
        assert_eq!(agent.name, "Coder");
        assert_eq!(agent.model_tier, vec![0]);
        assert_eq!(agent.max_context_tokens, 4000);
        assert_eq!(agent.created_by, "preset");
    }

    #[test]
    fn personal_agents_override_presets() {
        let dir = tempfile::tempdir().unwrap();
        let preset = dir.path().join("agents");
        let personal = dir.path().join("personal").join("agents");
        std::fs::create_dir_all(&preset).unwrap();
        std::fs::create_dir_all(&personal).unwrap();
        std::fs::write(preset.join("a.yaml"), "name: A\nsystem_prompt: preset\n").unwrap();
        std::fs::write(
            personal.join("a.yaml"),
            "name: A\nsystem_prompt: personal\n",
        )
        .unwrap();

        let registry = AgentRegistry::load(dir.path());
        assert_eq!(
            registry.find_by_type("a").unwrap().system_prompt,
            "personal"
        );
    }

    #[test]
    fn tag_matching_picks_best_overlap() {
        let dir = tempfile::tempdir().unwrap();
        let agents_dir = dir.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("writer.yaml"),
            "name: Writer\ntags: [writing, docs]\n",
        )
        .unwrap();
        std::fs::write(
            agents_dir.join("devops.yaml"),
            "name: Devops\ntags: [ops, cloud, writing]\n",
        )
        .unwrap();

        let registry = AgentRegistry::load(dir.path());
        let match_tags = |tags: &[&str]| {
            registry
                .find_by_tags(&tags.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .map(|a| a.name.clone())
        };
        assert_eq!(match_tags(&["docs"]).as_deref(), Some("Writer"));
        assert_eq!(match_tags(&["writing", "cloud"]).as_deref(), Some("Devops"));
        assert_eq!(match_tags(&["unknown"]), None);
        assert_eq!(match_tags(&[]), None);
    }

    #[test]
    fn missing_general_falls_back_to_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::load(dir.path());
        let default = registry.get_default();
        assert_eq!(default.name, "General");
        assert!(default.system_prompt.contains("helpful assistant"));
    }
}
