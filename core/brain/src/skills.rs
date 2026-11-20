//! Registry for SKILL.md-based capability packs.
//!
//! Port of `brain/nerv/skills/registry.py` + `models.py`: skills load from
//! `skills/*/SKILL.md` folders with optional YAML frontmatter.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// A reusable skill package loaded from a SKILL.md folder.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillDefinition {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub owner_agents: Vec<String>,
    #[serde(default)]
    pub recommended_workflows: Vec<String>,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub source_path: String,
}

/// Load reusable skill packages from SKILL.md folders.
pub struct SkillRegistry {
    pub skills_dir: PathBuf,
    skills: Vec<SkillDefinition>,
}

#[cfg(test)]
mod tests;

use std::path::PathBuf;

impl SkillRegistry {
    pub fn new(project_root: &Path) -> Self {
        // Preset skills first, then the owner's personal ones (personal wins
        // on a name clash), mirroring the agent registry's precedence.
        let skills_dir = project_root.join("skills");
        let personal_dir = project_root.join("personal").join("skills");
        let mut registry = Self {
            skills_dir: skills_dir.clone(),
            skills: Vec::new(),
        };
        registry.load_dir(&skills_dir);
        registry.load_dir(&personal_dir);
        registry
    }

    /// Load skills from one directory into the registry (later dirs override
    /// earlier ones on a name clash). Clears are done by the caller — `new`
    /// starts from empty, and `import_skill` reloads additively.
    pub(crate) fn load_dir(&mut self, skills_dir: &Path) {
        if !skills_dir.exists() {
            return;
        }

        let mut paths: Vec<PathBuf> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(skills_dir) {
            for entry in entries.flatten() {
                let skill_file = entry.path().join("SKILL.md");
                if skill_file.exists() {
                    paths.push(skill_file);
                }
            }
        }
        paths.sort();
        for path in paths {
            let skill = parse_skill_file(&path);
            self.skills
                .retain(|s| s.name.to_lowercase() != skill.name.to_lowercase());
            self.skills.push(skill);
        }
    }

    /// Return all loaded skills.
    pub fn list_skills(&self) -> Vec<SkillDefinition> {
        self.skills.clone()
    }

    /// Find a skill by name.
    pub fn find(&self, name: &str) -> Option<SkillDefinition> {
        self.skills
            .iter()
            .find(|s| s.name.to_lowercase() == name.trim().to_lowercase())
            .cloned()
    }

    /// Return skills relevant to an agent (owner or tag match).
    pub fn find_for_agent(&self, agent_name: &str, agent_tags: &[String]) -> Vec<SkillDefinition> {
        let agent_name = agent_name.trim().to_lowercase();
        let agent_tags: Vec<String> = agent_tags.iter().map(|t| t.to_lowercase()).collect();
        self.skills
            .iter()
            .filter(|skill| {
                let owners: Vec<String> = skill
                    .owner_agents
                    .iter()
                    .map(|o| o.to_lowercase())
                    .collect();
                let tags: Vec<String> = skill.tags.iter().map(|t| t.to_lowercase()).collect();
                owners.contains(&agent_name) || agent_tags.iter().any(|tag| tags.contains(tag))
            })
            .cloned()
            .collect()
    }

    /// Render a compact skill summary for prompt injection.
    pub fn render_catalog(
        &self,
        agent_name: &str,
        agent_tags: &[String],
        max_items: usize,
    ) -> String {
        let skills = self.find_for_agent(agent_name, agent_tags);
        if skills.is_empty() {
            return String::new();
        }

        let mut lines = vec!["[Skill Library]".to_string()];
        for skill in skills.iter().take(max_items) {
            lines.push(format!("- {}: {}", skill.name, skill.description));
            if !skill.recommended_workflows.is_empty() {
                lines.push(format!(
                    "  Recommended workflows: {}",
                    skill
                        .recommended_workflows
                        .iter()
                        .take(4)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        lines.push(
            "- Skills are reusable guidance packs. Use them to choose or design \
             workflows more consistently."
                .to_string(),
        );
        lines.join("\n")
    }
}

/// Parse a SKILL.md file with optional YAML frontmatter.
pub(crate) fn parse_skill_file(path: &Path) -> SkillDefinition {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    skill_from_markdown(&raw, || {
        path.parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    })
    .unwrap_or_default()
}

/// Parse a SKILL.md-formatted markdown string (optional YAML frontmatter).
/// `fallback_name` supplies the name when the frontmatter has none.
pub(crate) fn skill_from_markdown(
    raw: &str,
    fallback_name: impl Fn() -> String,
) -> Option<SkillDefinition> {
    let mut metadata: serde_yaml::Value = serde_yaml::Value::Null;
    let body;
    if raw.starts_with("---\n") {
        let mut parts = raw.splitn(3, "---\n");
        parts.next();
        let frontmatter = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("");
        metadata = serde_yaml::from_str(frontmatter).unwrap_or(serde_yaml::Value::Null);
        body = rest.trim().to_string();
    } else {
        body = raw.trim().to_string();
    }

    let get_list = |key: &str| -> Vec<String> {
        metadata
            .get(key)
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let get_str = |key: &str| -> String {
        metadata
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    let name = if get_str("name").is_empty() {
        fallback_name()
    } else {
        get_str("name")
    };

    Some(SkillDefinition {
        name,
        description: get_str("description"),
        tags: get_list("tags"),
        owner_agents: get_list("owner_agents"),
        recommended_workflows: get_list("recommended_workflows"),
        body,
        source_path: String::new(),
    })
}
