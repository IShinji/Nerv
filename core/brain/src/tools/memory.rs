//! Memory tools: searching past conversations and reading long-term facts.
//!
//! Port of `brain/nerv/tools/memory.py` against the rust MemoryManager. The
//! scope comes from the turn (`LoopContext.scope`) — a tool call reads the
//! conversation scope the current request belongs to and never another
//! sender's history.

use serde_json::{json, Value};

use super::helpers::truncate;
use super::helpers::MAX_TOOL_OUTPUT_CHARS;
use super::registry::{ToolContext, ToolDefinition, ToolResult};
use crate::memory::MemoryManager;

/// Enough of a message to judge relevance without flooding the context window.
const SNIPPET_CHARS: usize = 400;

fn memory(ctx: &ToolContext, scope: &str) -> MemoryManager {
    MemoryManager::new(&ctx.project_root, scope)
}

pub fn search_memory() -> ToolDefinition {
    ToolDefinition {
        name: "search_memory",
        description: "Search this conversation's own past messages by keyword. Use it to recall something older than the recent history already in context — a decision, a name, a link mentioned days or weeks ago. Optionally bound by date (YYYY-MM-DD) or restrict to one speaker.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "limit": {"type": "integer"},
                "since": {"type": "string"},
                "until": {"type": "string"},
                "role": {"type": "string"}
            },
            "required": ["query"]
        }),
        func: |ctx, args| {
            let ctx = ToolContext {
                project_root: ctx.project_root.clone(),
                workspace_root: ctx.workspace_root.clone(),
                brain: ctx.brain.clone(),
                scope: ctx.scope.clone(),
            };
            let args = args.clone();
            Box::pin(async move { search_memory_impl(&ctx, &args) })
        },
    }
}

fn search_memory_impl(ctx: &ToolContext, args: &Value) -> ToolResult {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.trim().is_empty() {
        return ToolResult::error("Error: query must not be empty.");
    }
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10);
    let limit = (limit.max(1) as usize).min(50);
    let since = args.get("since").and_then(|v| v.as_str()).unwrap_or("");
    let until = args.get("until").and_then(|v| v.as_str()).unwrap_or("");
    let role = args.get("role").and_then(|v| v.as_str()).unwrap_or("");

    let manager = memory(ctx, &ctx.scope);
    let hits = manager.search_messages(query, limit, since, until, role);

    if hits.is_empty() {
        return ToolResult::ok(format!("No past messages match {query:?}."));
    }

    let mut lines = vec![
        format!("{} match(es) for {query:?}, best first:", hits.len()),
        String::new(),
    ];
    for hit in hits {
        let stamp = hit.message.timestamp.format("%H:%M");
        let mut snippet = hit
            .message
            .content
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if snippet.chars().count() > SNIPPET_CHARS {
            snippet = format!(
                "{}…",
                snippet.chars().take(SNIPPET_CHARS).collect::<String>()
            );
        }
        lines.push(format!(
            "[{} {stamp}] {}: {snippet}",
            hit.date, hit.message.role
        ));
    }

    ToolResult::ok(truncate(
        &lines.join("\n"),
        MAX_TOOL_OUTPUT_CHARS,
        "search_memory",
    ))
}

pub fn get_fact_section() -> ToolDefinition {
    ToolDefinition {
        name: "get_fact_section",
        description: "Read one section of the long-term facts file in full. The system prompt lists only section titles once the file grows large; use this to pull back the one you need. Call with no title to list them.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "title": {"type": "string"}
            },
            "required": []
        }),
        func: |ctx, args| {
            let ctx = ToolContext {
                project_root: ctx.project_root.clone(),
                workspace_root: ctx.workspace_root.clone(),
                brain: ctx.brain.clone(),
                scope: ctx.scope.clone(),
            };
            let title = args
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Box::pin(async move { get_fact_section_impl(&ctx, &title) })
        },
    }
}

fn get_fact_section_impl(ctx: &ToolContext, title: &str) -> ToolResult {
    let memory = memory(ctx, &ctx.scope);
    if title.trim().is_empty() {
        let outline = memory.facts_outline();
        if outline.is_empty() {
            return ToolResult::ok("No long-term facts are recorded yet.");
        }
        return ToolResult::ok(format!("Fact sections:\n{outline}"));
    }

    let body = memory.read_fact_section(title);
    if body.is_empty() {
        let outline = memory.facts_outline();
        return ToolResult::error(format!(
            "No fact section titled {title:?}.{}",
            if outline.is_empty() {
                String::new()
            } else {
                format!("\nAvailable:\n{outline}")
            }
        ));
    }
    ToolResult::ok(truncate(
        &format!("## {}\n{body}", title.trim()),
        MAX_TOOL_OUTPUT_CHARS,
        "get_fact_section",
    ))
}
