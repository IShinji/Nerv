//! Knowledge tools: the workflow library, skills, and review queue.
//!
//! Port of the registry-backed half of `brain/nerv/tools/knowledge.py`.
//! `execute_workflow` (WorkflowExecutor), `mcp_status` and `mcp_presets`
//! (MCP manager) and `send_email` stay python-served until those migrate.

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
