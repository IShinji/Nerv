//! Everyday productivity tools: local calendar (ICS) and notes (Markdown).
//!
//! Port of the file-based half of `brain/nerv/tools/productivity.py`.
//! `send_email` routes through the MCP
//! manager, which has not migrated.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::registry::{ToolContext, ToolDefinition, ToolResult};

fn calendar_path(project_root: &Path) -> PathBuf {
    let path = project_root.join("personal").join("calendar.ics");
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/")));
    path
}

fn notes_dir(project_root: &Path) -> PathBuf {
    let path = project_root.join("personal").join("notes");
    let _ = std::fs::create_dir_all(&path);
    path
}

fn slugify_note(title: &str) -> String {
    let slug: String = title
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "note".to_string()
    } else {
        slug
    }
}

/// Normalize an ISO-ish datetime to an ICS timestamp (best effort).
fn to_ics_dt(value: &str) -> String {
    let value = value.trim();
    if let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M") {
        return parsed.format("%Y%m%dT%H%M%S").to_string();
    }
    if let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S") {
        return parsed.format("%Y%m%dT%H%M%S").to_string();
    }
    if let Ok(parsed) = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return parsed.format("%Y%m%dT000000").to_string();
    }
    let digits_only: String = value
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == 'T')
        .collect();
    if digits_only.is_empty() {
        value.to_string()
    } else {
        digits_only
    }
}

pub fn calendar_add() -> ToolDefinition {
    ToolDefinition {
        name: "calendar_add",
        description: "Add an event to the local calendar. start/end are ISO datetimes like '2026-06-01T09:00'. Returns confirmation.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "start": {"type": "string"},
                "end": {"type": "string"},
                "notes": {"type": "string"}
            },
            "required": ["title", "start"]
        }),
        func: |ctx, args| {
            let path = calendar_path(&ctx.project_root);
            let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let start = args.get("start").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let end = args.get("end").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let notes = args.get("notes").and_then(|v| v.as_str()).unwrap_or("").to_string();
            Box::pin(async move { calendar_add_impl(&path, &title, &start, &end, &notes) })
        },
    }
}

fn calendar_add_impl(path: &Path, title: &str, start: &str, end: &str, notes: &str) -> ToolResult {
    let uid = format!(
        "{}-{}@nerv",
        chrono::Local::now().format("%Y%m%dT%H%M%S"),
        slugify_note(title)
    );
    let dtstart = to_ics_dt(start);
    let dtend = if end.is_empty() {
        dtstart.clone()
    } else {
        to_ics_dt(end)
    };

    let mut lines = vec![
        "BEGIN:VEVENT".to_string(),
        format!("UID:{uid}"),
        format!("DTSTAMP:{}", chrono::Local::now().format("%Y%m%dT%H%M%S")),
        format!("DTSTART:{dtstart}"),
        format!("DTEND:{dtend}"),
        format!("SUMMARY:{title}"),
    ];
    if !notes.is_empty() {
        lines.push(format!("DESCRIPTION:{notes}"));
    }
    lines.push("END:VEVENT".to_string());
    let block = lines.join("\n") + "\n";

    let write_result = if !path.exists() {
        let header = "BEGIN:VCALENDAR\nVERSION:2.0\nPRODID:-//Nerv//EN\n";
        std::fs::write(path, format!("{header}{block}END:VCALENDAR\n"))
    } else {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let updated = if text.contains("END:VCALENDAR") {
            text.replace("END:VCALENDAR", &format!("{block}END:VCALENDAR"))
        } else {
            text + &block
        };
        std::fs::write(path, updated)
    };
    match write_result {
        Ok(()) => ToolResult::ok(format!("Added calendar event '{title}' at {start}.")),
        Err(e) => ToolResult::error(format!("Failed to write the calendar: {e}")),
    }
}

pub fn calendar_list() -> ToolDefinition {
    ToolDefinition {
        name: "calendar_list",
        description: "List upcoming events from the local calendar.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer"}
            },
            "required": []
        }),
        func: |ctx, args| {
            let path = calendar_path(&ctx.project_root);
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            Box::pin(async move { calendar_list_impl(&path, limit) })
        },
    }
}

fn calendar_list_impl(path: &Path, limit: usize) -> ToolResult {
    if !path.exists() {
        return ToolResult::ok("The calendar is empty.");
    }

    let mut events: Vec<(String, String)> = Vec::new();
    let mut summary = String::new();
    let mut start = String::new();
    for line in std::fs::read_to_string(path).unwrap_or_default().lines() {
        if let Some(rest) = line.strip_prefix("SUMMARY:") {
            summary = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("DTSTART:") {
            start = rest.to_string();
        } else if line.starts_with("END:VEVENT") && !summary.is_empty() {
            events.push((start.clone(), summary.clone()));
            summary = String::new();
            start = String::new();
        }
    }

    if events.is_empty() {
        return ToolResult::ok("The calendar is empty.");
    }

    events.sort();
    let lines: Vec<String> = events
        .into_iter()
        .take(limit)
        .map(|(start, summary)| format!("- {start}: {summary}"))
        .collect();
    ToolResult::ok(format!("Upcoming events:\n{}", lines.join("\n")))
}

pub fn note_write() -> ToolDefinition {
    ToolDefinition {
        name: "note_write",
        description: "Create or overwrite a Markdown note by title in personal/notes.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["title", "content"]
        }),
        func: |ctx, args| {
            let dir = notes_dir(&ctx.project_root);
            let title = args
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Box::pin(async move {
                let path = dir.join(format!("{}.md", slugify_note(&title)));
                match std::fs::write(&path, format!("# {title}\n\n{content}\n")) {
                    Ok(()) => ToolResult::ok(format!("Saved note '{title}'.")),
                    Err(e) => ToolResult::error(format!("Failed to save the note: {e}")),
                }
            })
        },
    }
}

pub fn note_list() -> ToolDefinition {
    ToolDefinition {
        name: "note_list",
        description: "List saved notes by title.",
        requires_confirmation: false,
        parameters: json!({"type": "object", "properties": {}, "required": []}),
        func: |ctx, _args| {
            let dir = notes_dir(&ctx.project_root);
            Box::pin(async move {
                let mut titles: Vec<String> = Vec::new();
                let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
                    .map(|entries| {
                        entries
                            .flatten()
                            .map(|e| e.path())
                            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
                            .collect()
                    })
                    .unwrap_or_default();
                paths.sort();
                for path in paths {
                    let first_line = std::fs::read_to_string(&path)
                        .unwrap_or_default()
                        .lines()
                        .next()
                        .unwrap_or("")
                        .to_string();
                    let title = first_line.trim_start_matches('#').trim().to_string();
                    let stem = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    titles.push(if title.is_empty() {
                        stem.clone()
                    } else {
                        title
                    });
                }
                if titles.is_empty() {
                    return ToolResult::ok("No notes saved yet.");
                }
                titles.sort();
                ToolResult::ok(format!(
                    "Notes:\n{}",
                    titles
                        .iter()
                        .map(|t| format!("- {t}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ))
            })
        },
    }
}

pub fn note_read() -> ToolDefinition {
    ToolDefinition {
        name: "note_read",
        description: "Read the contents of a saved note by title.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "title": {"type": "string"}
            },
            "required": ["title"]
        }),
        func: |ctx, args| {
            let dir = notes_dir(&ctx.project_root);
            let title = args
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Box::pin(async move {
                let path = dir.join(format!("{}.md", slugify_note(&title)));
                if !path.exists() {
                    return ToolResult::error(format!("No note titled '{title}'."));
                }
                match std::fs::read_to_string(&path) {
                    Ok(content) => ToolResult::ok(content),
                    Err(e) => ToolResult::error(format!("Failed to read the note: {e}")),
                }
            })
        },
    }
}

/// Send an email through a configured MCP email server (capability
/// `email.send`); degrades gracefully when none is configured.
pub fn send_email() -> ToolDefinition {
    ToolDefinition {
        name: "send_email",
        description: "Send an email. Requires a configured email MCP server.",
        requires_confirmation: true,
        parameters: json!({
            "type": "object",
            "properties": {
                "to": {"type": "string"},
                "subject": {"type": "string"},
                "body": {"type": "string"}
            },
            "required": ["to", "subject", "body"]
        }),
        func: |ctx, args| {
            let ctx = ToolContext {
                project_root: ctx.project_root.clone(),
                workspace_root: ctx.workspace_root.clone(),
                scope: ctx.scope.clone(),
                confirmed: ctx.confirmed.clone(),
            };
            let args = args.clone();
            Box::pin(async move { send_email_impl(&ctx, &args).await })
        },
    }
}

async fn send_email_impl(ctx: &ToolContext, args: &Value) -> ToolResult {
    let manager = crate::mcp::get_mcp_manager(&ctx.project_root);
    let mut manager = manager.lock().await;
    if !manager.has_capability_binding("email.send") {
        return ToolResult::error(
            "No email provider is configured. Add an email MCP server under \
             `mcp.servers` in config (bound to capability `email.send`) to \
             enable sending. Drafted message was not sent.",
        );
    }
    let arguments = json!({
        "to": args.get("to").and_then(|v| v.as_str()).unwrap_or(""),
        "subject": args.get("subject").and_then(|v| v.as_str()).unwrap_or(""),
        "body": args.get("body").and_then(|v| v.as_str()).unwrap_or(""),
    });
    match manager
        .invoke_capability_action("email.send", "send", &arguments)
        .await
    {
        Ok(Some(result)) => ToolResult {
            content: result.content,
            is_error: result.is_error,
            is_pending: false,
        },
        Ok(None) => {
            ToolResult::error("The configured email server could not handle the send action.")
        }
        Err(e) => ToolResult::error(format!("Email provider failed: {e}")),
    }
}
