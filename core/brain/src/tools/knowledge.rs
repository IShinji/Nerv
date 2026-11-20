//! Knowledge tools: the workflow library, skills, and review queue.
//!
//! Port of the registry-backed half of `brain/nerv/tools/knowledge.py`.
//! `execute_workflow` (WorkflowExecutor), `mcp_status` and `mcp_presets`
//! (MCP manager) and `send_email` stay python-served until those migrate.

use std::path::Path;

use serde_json::json;

use super::helpers::{parse_csv_or_lines, truncate};
use super::registry::{ToolDefinition, ToolResult};
use crate::skills::SkillRegistry;
use crate::workflows::{ReviewQueue, WorkflowDefinition, WorkflowRegistry};

pub fn list_workflows() -> ToolDefinition {
    ToolDefinition {
        name: "list_workflows",
        description: "List shared workflows relevant to an agent or task domain. Use this before drafting a new workflow.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "agent_name": {"type": "string"}
            },
            "required": []
        }),
        func: |ctx, args| {
            let agent_name = args
                .get("agent_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let registry = WorkflowRegistry::new(&ctx.project_root);
            Box::pin(async move {
                let mut workflows = registry.list_workflows();
                if !agent_name.trim().is_empty() {
                    let target = agent_name.trim().to_lowercase();
                    workflows.retain(|workflow| {
                        workflow
                            .owner_agents
                            .iter()
                            .any(|owner| owner.to_lowercase() == target)
                    });
                }

                if workflows.is_empty() {
                    return ToolResult::ok("No shared workflows found.");
                }

                let mut lines = vec!["Shared workflows:".to_string()];
                for workflow in workflows {
                    let owners = if workflow.owner_agents.is_empty() {
                        "all agents".to_string()
                    } else {
                        workflow.owner_agents.join(", ")
                    };
                    lines.push(format!("- {}: {}", workflow.name, workflow.description));
                    lines.push(format!("  Owners: {owners}"));
                    if !workflow.tools.is_empty() {
                        lines.push(format!("  Tools: {}", workflow.tools.join(", ")));
                    }
                }
                ToolResult::ok(lines.join("\n"))
            })
        },
    }
}

pub fn get_workflow() -> ToolDefinition {
    ToolDefinition {
        name: "get_workflow",
        description:
            "Read a shared workflow definition by name, including steps and success criteria.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"}
            },
            "required": ["name"]
        }),
        func: |ctx, args| {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let registry = WorkflowRegistry::new(&ctx.project_root);
            Box::pin(async move {
                let Some(workflow) = registry.find(&name) else {
                    return ToolResult::error(format!("Workflow not found: {name}"));
                };
                let mut lines = vec![
                    format!("Workflow: {}", workflow.name),
                    workflow.description.clone(),
                ];
                if !workflow.steps.is_empty() {
                    lines.push("Steps:".to_string());
                    lines.extend(workflow.steps.iter().map(|step| format!("- {step}")));
                }
                if !workflow.success_criteria.is_empty() {
                    lines.push("Success criteria:".to_string());
                    lines.extend(
                        workflow
                            .success_criteria
                            .iter()
                            .map(|item| format!("- {item}")),
                    );
                }
                ToolResult::ok(lines.join("\n"))
            })
        },
    }
}

pub fn propose_workflow() -> ToolDefinition {
    ToolDefinition {
        name: "propose_workflow",
        description: "Draft a new workflow proposal and send it to the review queue. Input list fields as comma-separated or newline-separated text.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "description": {"type": "string"},
                "tags": {"type": "string"},
                "owner_agents": {"type": "string"},
                "tools": {"type": "string"},
                "steps": {"type": "string"},
                "success_criteria": {"type": "string"}
            },
            "required": ["name", "description"]
        }),
        func: |ctx, args| {
            let get = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let project_root = ctx.project_root.clone();
            Box::pin(async move {
                let workflow = WorkflowDefinition {
                    name: get("name").trim().to_string(),
                    description: get("description").trim().to_string(),
                    tags: parse_csv_or_lines(&get("tags")),
                    owner_agents: parse_csv_or_lines(&get("owner_agents")),
                    tools: parse_csv_or_lines(&get("tools")),
                    steps: parse_csv_or_lines(&get("steps")),
                    success_criteria: parse_csv_or_lines(&get("success_criteria")),
                    created_by: "agent".to_string(),
                    review_status: "pending".to_string(),
                    ..Default::default()
                };

                let review_item =
                    ReviewQueue::new(&project_root).submit_workflow(workflow, "agent");
                ToolResult::ok(format!(
                    "Workflow proposal queued for review: {}\nName: {}\nAsk the user to review it with \"show workflow reviews\", \"approve workflow {}\", or \"reject workflow {}\".",
                    review_item.review_id, review_item.workflow.name, review_item.review_id, review_item.review_id
                ))
            })
        },
    }
}

pub fn list_skills() -> ToolDefinition {
    ToolDefinition {
        name: "list_skills",
        description: "List shared skills relevant to an agent or task domain.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "agent_name": {"type": "string"}
            },
            "required": []
        }),
        func: |ctx, args| {
            let agent_name = args
                .get("agent_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let registry = SkillRegistry::new(&ctx.project_root);
            Box::pin(async move {
                let mut skills = registry.list_skills();
                if !agent_name.trim().is_empty() {
                    let target = agent_name.trim().to_lowercase();
                    skills.retain(|skill| {
                        skill
                            .owner_agents
                            .iter()
                            .any(|owner| owner.to_lowercase() == target)
                    });
                }

                if skills.is_empty() {
                    return ToolResult::ok("No skills found.");
                }

                let mut lines = vec!["Skills:".to_string()];
                for skill in skills {
                    lines.push(format!("- {}: {}", skill.name, skill.description));
                    if !skill.recommended_workflows.is_empty() {
                        lines.push(format!(
                            "  Recommended workflows: {}",
                            skill.recommended_workflows.join(", ")
                        ));
                    }
                }
                ToolResult::ok(lines.join("\n"))
            })
        },
    }
}

pub fn get_skill() -> ToolDefinition {
    ToolDefinition {
        name: "get_skill",
        description: "Read the details of a specific skill by name.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"}
            },
            "required": ["name"]
        }),
        func: |ctx, args| {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let registry = SkillRegistry::new(&ctx.project_root);
            Box::pin(async move {
                let Some(skill) = registry.find(&name) else {
                    return ToolResult::error(format!("Skill not found: {name}"));
                };
                let body = truncate(skill.body.trim(), 3000, "get_skill");
                let mut lines = vec![format!("Skill: {}", skill.name), skill.description.clone()];
                if !skill.recommended_workflows.is_empty() {
                    lines.push(format!(
                        "Recommended workflows: {}",
                        skill.recommended_workflows.join(", ")
                    ));
                }
                if !body.is_empty() {
                    lines.push("Body:".to_string());
                    lines.push(body);
                }
                ToolResult::ok(lines.join("\n"))
            })
        },
    }
}

pub fn list_review_queue() -> ToolDefinition {
    ToolDefinition {
        name: "list_review_queue",
        description: "List pending or historical workflow review items.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "status": {"type": "string"}
            },
            "required": []
        }),
        func: |ctx, args| {
            let status = args
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending")
                .trim()
                .to_string();
            let queue = ReviewQueue::new(&ctx.project_root);
            Box::pin(async move {
                let items = queue.list_reviews(&status);
                if items.is_empty() {
                    return ToolResult::ok(format!(
                        "No workflow review items found for status: {status}"
                    ));
                }
                let mut lines = vec![format!("Workflow review items ({status}):")];
                for item in items {
                    lines.push(format!(
                        "- {}: {} [{}] - {}",
                        item.review_id, item.workflow.name, item.status, item.workflow.description
                    ));
                }
                ToolResult::ok(lines.join("\n"))
            })
        },
    }
}

/// List builtin MCP presets, or render one preset's config snippet.
pub fn mcp_presets() -> ToolDefinition {
    ToolDefinition {
        name: "mcp_presets",
        description: "Inspect builtin MCP integrations or generate a ready-to-copy config snippet for a known server.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "preset_name": {"type": "string"}
            },
            "required": []
        }),
        func: |_ctx, args| {
            let preset_name = args
                .get("preset_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            Box::pin(async move {
                use crate::mcp::presets::{
                    get_builtin_mcp_preset, list_builtin_mcp_presets, render_mcp_preset_snippet,
                };
                if preset_name.is_empty() {
                    let presets = list_builtin_mcp_presets();
                    if presets.is_empty() {
                        return ToolResult::ok("No builtin MCP presets are available.");
                    }
                    let mut lines = vec!["Builtin MCP presets:".to_string()];
                    for preset in presets {
                        let capabilities = if preset.capabilities.is_empty() {
                            "none".to_string()
                        } else {
                            preset.capabilities.join(", ")
                        };
                        lines.push(format!("- {}: {}", preset.name, preset.description));
                        lines.push(format!("  Capabilities: {capabilities}"));
                        lines.push(format!("  Source: {}", preset.source_url));
                    }
                    return ToolResult::ok(lines.join("\n"));
                }
                let Some(preset) = get_builtin_mcp_preset(&preset_name) else {
                    return ToolResult::error(format!("Unknown MCP preset: {preset_name}"));
                };
                let mut lines = vec![
                    format!("Preset: {}", preset.name),
                    preset.description.to_string(),
                    format!("Source: {}", preset.source_url),
                ];
                if !preset.default_action_map.is_empty() {
                    let mut mapped: Vec<String> = preset
                        .default_action_map
                        .iter()
                        .map(|(action, tool)| format!("{action}->{tool}"))
                        .collect();
                    mapped.sort();
                    lines.push(format!("Mapped actions: {}", mapped.join(", ")));
                }
                if !preset.notes.is_empty() {
                    lines.push("Notes:".to_string());
                    lines.extend(preset.notes.iter().map(|n| format!("- {n}")));
                }
                lines.push("Config snippet:".to_string());
                lines.push(render_mcp_preset_snippet(preset, "chrome"));
                ToolResult::ok(lines.join("\n"))
            })
        },
    }
}

/// Report MCP server configuration and health.
pub fn mcp_status() -> ToolDefinition {
    ToolDefinition {
        name: "mcp_status",
        description: "Inspect configured MCP servers, their health, bound capabilities, and available remote tools. Optionally pass a specific server name.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "server_name": {"type": "string"}
            },
            "required": []
        }),
        func: |ctx, args| {
            let server_name = args
                .get("server_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let project_root = ctx.project_root.clone();
            Box::pin(async move {
                let manager = crate::mcp::get_mcp_manager(&project_root);
                let statuses = match manager.lock().await.get_status(&server_name).await {
                    Ok(statuses) => statuses,
                    Err(e) => return ToolResult::error(e.to_string()),
                };
                if statuses.is_empty() {
                    return ToolResult::ok("No MCP servers are configured.");
                }
                let mut lines = vec!["MCP server status:".to_string()];
                for status in statuses {
                    let mut health = if status.healthy { "healthy" } else { "unhealthy" };
                    if !status.enabled {
                        health = "disabled";
                    }
                    lines.push(format!("- {}: {health}", status.name));
                    lines.push(format!("  Transport: {}", status.transport));
                    let command = if status.command.is_empty() {
                        "(unset)"
                    } else {
                        &status.command
                    };
                    lines.push(format!("  Command: {command}"));
                    let args = if status.args.is_empty() {
                        "(none)".to_string()
                    } else {
                        status.args.join(" ")
                    };
                    lines.push(format!("  Args: {args}"));
                    if !status.preset.is_empty() {
                        lines.push(format!("  Preset: {}", status.preset));
                    }
                    let capabilities = if status.capabilities.is_empty() {
                        "none".to_string()
                    } else {
                        status.capabilities.join(", ")
                    };
                    lines.push(format!("  Capabilities: {capabilities}"));
                    if !status.action_map.is_empty() {
                        let mut mapped: Vec<String> = status
                            .action_map
                            .iter()
                            .map(|(a, t)| format!("{a}->{t}"))
                            .collect();
                        mapped.sort();
                        lines.push(format!("  Action map: {}", mapped.join(", ")));
                    }
                    if !status.available_tools.is_empty() {
                        lines.push(format!("  Remote tools: {}", status.available_tools.join(", ")));
                    }
                    if !status.error.is_empty() {
                        lines.push(format!("  Error: {}", status.error));
                    }
                }
                ToolResult::ok(lines.join("\n"))
            })
        },
    }
}

/// One-click skill import (PRD Phase 2 生态格式兼容转换器): fetch a
/// SKILL.md-formatted skill from a URL (a raw GitHub URL, a ClawHub skill
/// endpoint or any URL serving the markdown) and save it into
/// `personal/skills/<slug>/SKILL.md` so the registry picks it up.
pub fn import_skill() -> ToolDefinition {
    ToolDefinition {
        name: "import_skill",
        description: "Import a skill pack from a URL (e.g. a ClawHub skill or a raw SKILL.md on GitHub) and install it into personal/skills for reuse. Args: url, name (optional slug override).",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "name": {"type": "string"}
            },
            "required": ["url"]
        }),
        func: |ctx, args| {
            let url = args
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let name_override = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let project_root = ctx.project_root.clone();
            Box::pin(async move {
                import_skill_impl(&project_root, &url, &name_override).await
            })
        },
    }
}

/// Filesystem-safe slug for a skill's install directory: lowercase, any
/// separator run collapsed to a single `-`.
pub(crate) fn skill_slug(name: &str) -> String {
    name.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

async fn import_skill_impl(project_root: &Path, url: &str, name_override: &str) -> ToolResult {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return ToolResult::error("Error: url must be an http(s) URL.");
    }
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(client) => client,
        Err(e) => return ToolResult::error(format!("Error: http client: {e}")),
    };
    let body = match client.get(url).send().await {
        Ok(response) => match response.text().await {
            Ok(text) => text,
            Err(e) => return ToolResult::error(format!("Error: failed to read {url}: {e}")),
        },
        Err(e) => return ToolResult::error(format!("Error: failed to fetch {url}: {e}")),
    };

    let fallback = || name_override.to_string();
    let Some(mut skill) = crate::skills::skill_from_markdown(&body, fallback) else {
        return ToolResult::error(
            "Error: the URL did not return a SKILL.md-formatted document \
             (expected optional YAML frontmatter with name/description).",
        );
    };
    if !name_override.is_empty() {
        skill.name = name_override.to_string();
    }
    if skill.name.trim().is_empty() {
        return ToolResult::error(
            "Error: the skill has no name (frontmatter `name:` or the `name` argument).",
        );
    }

    // Filesystem-safe slug: lowercase, separators collapsed to `-`.
    let slug = skill_slug(&skill.name);
    if slug.is_empty() {
        return ToolResult::error("Error: the skill name cannot be turned into a directory name.");
    }

    let dir = project_root.join("personal").join("skills").join(&slug);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return ToolResult::error(format!("Error: could not create {dir:?}: {e}"));
    }
    let path = dir.join("SKILL.md");
    if let Err(e) = std::fs::write(&path, &body) {
        return ToolResult::error(format!("Error: could not write {path:?}: {e}"));
    }

    ToolResult::ok(format!(
        "Skill '{}' imported into {} (description: {}). It is available on the \
         next turn; `list_skills` shows it after the registry reloads.",
        skill.name,
        path.display(),
        if skill.description.is_empty() {
            "(none)"
        } else {
            &skill.description
        }
    ))
}
