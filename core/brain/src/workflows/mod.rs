//! Workflow registry and review queue.
//!
//! Port of `brain/nerv/workflows/registry.py` + `models.py`. Workflows load
//! from the shared library (`workflows/`) and the user's personal library
//! (`personal/workflows/`); proposals go through a file-backed review queue
//! whose YAML layout is the interface to the python side (the review
//! approve/reject commands still run there until the orchestrator migrates).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Reusable workflow definition stored on disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub owner_agents: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub success_criteria: Vec<String>,
    #[serde(default = "default_created_by")]
    pub created_by: String,
    #[serde(default = "default_review_status")]
    pub review_status: String,
    #[serde(default)]
    pub source_path: String,
}

fn default_created_by() -> String {
    "preset".to_string()
}

fn default_review_status() -> String {
    "approved".to_string()
}

/// A workflow proposal waiting for user approval.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkflowReviewItem {
    pub review_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub proposed_by: String,
    #[serde(default)]
    pub created_at: String,
    pub workflow: WorkflowDefinition,
    #[serde(default)]
    pub reviewer_notes: String,
    #[serde(default)]
    pub source_path: String,
}

/// Review ids and slugged names land directly in filesystem paths, so
/// anything outside this charset (separators and ".." in particular) is
/// rejected before a path is ever built from them.
fn require_safe_path_id(value: &str, what: &str) -> Result<(), String> {
    let ok = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok {
        Ok(())
    } else {
        Err(format!("unsafe {what}: {value:?}"))
    }
}

/// Convert a label into a filesystem-safe slug.
pub fn slugify(value: &str) -> String {
    static NON_SLUG: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"[^a-z0-9]+").expect("static regex"));
    static RUN: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"_+").expect("static regex"));

    let lowered = value.trim().to_lowercase();
    let normalized = NON_SLUG.replace_all(&lowered, "_");
    let normalized = RUN.replace_all(&normalized, "_");
    let normalized = normalized.trim_matches('_');
    if normalized.is_empty() {
        "workflow".to_string()
    } else {
        normalized.to_string()
    }
}

use regex::Regex;
use tracing::error;

/// Serialize a workflow for YAML persistence (python's `workflow_to_dict`).
fn workflow_to_dict(workflow: &WorkflowDefinition) -> serde_yaml::Value {
    serde_yaml::to_value(workflow).unwrap_or(serde_yaml::Value::Null)
}

/// Load approved workflows from the shared workflow library.
pub struct WorkflowRegistry {
    pub workflows_dir: PathBuf,
    workflows: Vec<WorkflowDefinition>,
}

impl WorkflowRegistry {
    pub fn new(project_root: &Path) -> Self {
        let workflows_dir = project_root.join("workflows");
        let mut registry = Self {
            workflows_dir,
            workflows: Vec::new(),
        };
        registry.reload(project_root);
        registry
    }

    /// Reload workflows from disk.
    pub fn reload(&mut self, project_root: &Path) {
        self.workflows = Vec::new();
        // Load from the shared (public) workflows directory...
        self.load_dir(&self.workflows_dir.clone());
        // ...then the personal (user-configured) workflows directory.
        let personal_dir = project_root.join("personal").join("workflows");
        self.load_dir(&personal_dir);
    }

    fn load_dir(&mut self, directory: &Path) {
        if !directory.exists() {
            return;
        }
        let mut paths: Vec<PathBuf> = walk_yaml(directory);
        paths.sort();
        for path in paths {
            let workflow = parse_workflow_file(&path);
            if workflow.review_status != "approved" {
                continue;
            }
            self.workflows
                .retain(|w| w.name.to_lowercase() != workflow.name.to_lowercase());
            self.workflows.push(workflow);
        }
    }

    /// Return all approved workflows.
    pub fn list_workflows(&self) -> Vec<WorkflowDefinition> {
        self.workflows.clone()
    }

    /// Find a workflow by name.
    pub fn find(&self, name: &str) -> Option<WorkflowDefinition> {
        self.workflows
            .iter()
            .find(|w| w.name.to_lowercase() == name.trim().to_lowercase())
            .cloned()
    }

    /// Return workflows relevant to a specific agent (owner or tag match).
    pub fn find_for_agent(
        &self,
        agent_name: &str,
        agent_tags: &[String],
    ) -> Vec<WorkflowDefinition> {
        let agent_name = agent_name.trim().to_lowercase();
        let agent_tags: Vec<String> = agent_tags.iter().map(|t| t.to_lowercase()).collect();
        self.workflows
            .iter()
            .filter(|workflow| {
                let owners: Vec<String> = workflow
                    .owner_agents
                    .iter()
                    .map(|o| o.to_lowercase())
                    .collect();
                let tags: Vec<String> = workflow.tags.iter().map(|t| t.to_lowercase()).collect();
                owners.contains(&agent_name) || agent_tags.iter().any(|tag| tags.contains(tag))
            })
            .cloned()
            .collect()
    }

    /// Render a compact workflow catalog for prompt injection.
    pub fn render_catalog(
        &self,
        agent_name: &str,
        agent_tags: &[String],
        max_items: usize,
    ) -> String {
        let workflows = self.find_for_agent(agent_name, agent_tags);
        if workflows.is_empty() {
            return String::new();
        }

        let mut lines = vec!["[Workflow Library]".to_string()];
        for workflow in workflows.iter().take(max_items) {
            let tools = if workflow.tools.is_empty() {
                "none".to_string()
            } else {
                workflow.tools.join(", ")
            };
            lines.push(format!("- {}: {}", workflow.name, workflow.description));
            lines.push(format!("  Tools: {tools}"));
            if !workflow.steps.is_empty() {
                lines.push(format!(
                    "  Steps: {}",
                    workflow
                        .steps
                        .iter()
                        .take(4)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" -> ")
                ));
            }
        }
        lines.push(
            "- Prefer an existing shared workflow when it clearly matches the task. \
             If no shared workflow fits, you may draft a new one and send it to the review queue."
                .to_string(),
        );
        lines.join("\n")
    }
}

fn walk_yaml(directory: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(directory) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(walk_yaml(&path));
            } else if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                found.push(path);
            }
        }
    }
    found
}

fn parse_workflow_file(path: &Path) -> WorkflowDefinition {
    let data: serde_yaml::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_yaml::from_str(&text).ok())
        .unwrap_or(serde_yaml::Value::Null);

    let get_list = |key: &str| -> Vec<String> {
        data.get(key)
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let get_str = |key: &str, default: &str| -> String {
        data.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    };

    WorkflowDefinition {
        name: data
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        description: get_str("description", ""),
        tags: get_list("tags"),
        owner_agents: get_list("owner_agents"),
        tools: get_list("tools"),
        steps: get_list("steps"),
        success_criteria: get_list("success_criteria"),
        created_by: get_str("created_by", "preset"),
        review_status: get_str("review_status", "approved"),
        source_path: path.display().to_string(),
    }
}

/// File-backed review queue for workflow proposals.
pub struct ReviewQueue {
    pub reviews_dir: PathBuf,
    pub workflows_dir: PathBuf,
}

impl ReviewQueue {
    pub fn new(project_root: &Path) -> Self {
        let reviews_dir = project_root.join("reviews").join("workflows");
        let workflows_dir = project_root.join("workflows");
        let _ = std::fs::create_dir_all(&reviews_dir);
        let _ = std::fs::create_dir_all(&workflows_dir);
        Self {
            reviews_dir,
            workflows_dir,
        }
    }

    /// List workflow review items, optionally filtered by status.
    pub fn list_reviews(&self, status: &str) -> Vec<WorkflowReviewItem> {
        let mut items = Vec::new();
        let mut paths: Vec<PathBuf> = walk_yaml(&self.reviews_dir);
        paths.sort();
        for path in paths {
            if let Some(item) = self.parse_review_file(&path) {
                if !status.is_empty() && item.status != status {
                    continue;
                }
                items.push(item);
            }
        }
        items
    }

    /// Get a review item by ID.
    pub fn get(&self, review_id: &str) -> Option<WorkflowReviewItem> {
        let path = self.reviews_dir.join(format!("{review_id}.yaml"));
        if !path.exists() {
            return None;
        }
        self.parse_review_file(&path)
    }

    /// Create a pending review item for a workflow proposal.
    pub fn submit_workflow(
        &self,
        mut workflow: WorkflowDefinition,
        proposed_by: &str,
    ) -> WorkflowReviewItem {
        let review_id = self.new_review_id(&workflow.name);
        workflow.review_status = "pending".to_string();

        let item = WorkflowReviewItem {
            review_id: review_id.clone(),
            status: "pending".to_string(),
            proposed_by: proposed_by.to_string(),
            created_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            workflow: workflow.clone(),
            reviewer_notes: String::new(),
            source_path: self
                .reviews_dir
                .join(format!("{review_id}.yaml"))
                .display()
                .to_string(),
        };
        self.write_review(&item);
        item
    }

    /// Approve a pending workflow and publish it to the shared library.
    pub fn approve_workflow(&self, review_id: &str) -> Result<WorkflowReviewItem, String> {
        let Some(mut item) = self.get(review_id) else {
            return Err(format!("Unknown workflow review: {review_id}"));
        };

        item.status = "approved".to_string();
        item.workflow.review_status = "approved".to_string();

        let slug = slugify(&item.workflow.name);
        require_safe_path_id(&slug, "workflow slug")
            .map_err(|e| format!("Failed to publish the workflow: {e}"))?;
        let target_path = self.workflows_dir.join(format!("{slug}.yaml"));
        let payload = workflow_to_dict(&item.workflow);
        let yaml_text = serde_yaml::to_string(&payload)
            .map_err(|e| format!("Failed to serialize the workflow: {e}"))?;
        std::fs::write(&target_path, yaml_text)
            .map_err(|e| format!("Failed to publish the workflow: {e}"))?;

        self.write_review(&item);
        Ok(item)
    }

    /// Reject a pending workflow proposal.
    pub fn reject_workflow(
        &self,
        review_id: &str,
        reviewer_notes: &str,
    ) -> Result<WorkflowReviewItem, String> {
        let Some(mut item) = self.get(review_id) else {
            return Err(format!("Unknown workflow review: {review_id}"));
        };
        item.status = "rejected".to_string();
        item.reviewer_notes = reviewer_notes.to_string();
        self.write_review(&item);
        Ok(item)
    }

    /// Generate a stable, readable review ID.
    fn new_review_id(&self, name: &str) -> String {
        let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
        format!("wf-{timestamp}-{}", slugify(name))
    }

    /// Parse a workflow review YAML file.
    fn parse_review_file(&self, path: &Path) -> Option<WorkflowReviewItem> {
        let data: serde_yaml::Value = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_yaml::from_str(&text).ok())
            .unwrap_or(serde_yaml::Value::Null);

        let get_str = |value: &serde_yaml::Value, key: &str, default: &str| -> String {
            value
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or(default)
                .to_string()
        };
        let get_list = |value: &serde_yaml::Value, key: &str| -> Vec<String> {
            value
                .get(key)
                .and_then(|v| v.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };

        let workflow_data = data
            .get("workflow")
            .cloned()
            .unwrap_or(serde_yaml::Value::Null);
        workflow_data.get("name").and_then(|v| v.as_str())?;

        let workflow = WorkflowDefinition {
            name: get_str(&workflow_data, "name", ""),
            description: get_str(&workflow_data, "description", ""),
            tags: get_list(&workflow_data, "tags"),
            owner_agents: get_list(&workflow_data, "owner_agents"),
            tools: get_list(&workflow_data, "tools"),
            steps: get_list(&workflow_data, "steps"),
            success_criteria: get_list(&workflow_data, "success_criteria"),
            created_by: get_str(&workflow_data, "created_by", "agent"),
            review_status: workflow_data
                .get("review_status")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| get_str(&data, "status", "pending")),
            source_path: get_str(&workflow_data, "source_path", ""),
        };

        Some(WorkflowReviewItem {
            review_id: get_str(&data, "review_id", ""),
            status: get_str(&data, "status", "pending"),
            proposed_by: get_str(&data, "proposed_by", "agent"),
            created_at: get_str(&data, "created_at", ""),
            workflow,
            reviewer_notes: get_str(&data, "reviewer_notes", ""),
            source_path: path.display().to_string(),
        })
    }

    /// Persist a review item back to disk.
    fn write_review(&self, item: &WorkflowReviewItem) {
        if let Err(e) = require_safe_path_id(&item.review_id, "review id") {
            error!("{e}");
            return;
        }
        let path = self.reviews_dir.join(format!("{}.yaml", item.review_id));
        let payload = serde_yaml::to_value(item).unwrap_or(serde_yaml::Value::Null);
        let _ = std::fs::write(&path, serde_yaml::to_string(&payload).unwrap_or_default());
    }
}

#[cfg(test)]
mod tests;
