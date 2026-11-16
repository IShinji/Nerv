//! User-facing rendering of suspended (pending-confirmation) actions.
//! Port of `_format_pending_action` / `_build_pending_action_reply` in
//! `brain/nerv/orchestrator/orchestrator.py`. The store itself stays in
//! python until the tools migrate; this side only renders what the python
//! `run_tool` / `prepare_turn` RPCs report.

pub use crate::tools::pending_store::PendingRecord;

/// Render a user-facing summary of a pending action.
pub fn format_pending_action(action: &PendingRecord) -> String {
    let mut summary = match action.tool_name.as_str() {
        "desktop_control" => {
            let verb = action
                .arguments
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("desktop action");
            let details: Vec<String> = ["application", "url", "text", "key", "modifiers"]
                .iter()
                .filter_map(|key| {
                    let value = action
                        .arguments
                        .get(*key)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim();
                    if value.is_empty() {
                        None
                    } else {
                        Some(format!("{key}={value}"))
                    }
                })
                .collect();
            let detail_text = if details.is_empty() {
                String::new()
            } else {
                format!(" ({})", details.join(", "))
            };
            format!(
                "Pending action #{id}: desktop_control -> {verb}{detail_text}",
                id = action.id
            )
        }
        "screenshot" => {
            let output_path = action
                .arguments
                .get("output_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let output_path = if output_path.is_empty() {
                "(auto path)"
            } else {
                output_path
            };
            format!(
                "Pending action #{id}: screenshot -> save to {output_path}",
                id = action.id
            )
        }
        "execute_workflow" => {
            let name = action
                .arguments
                .get("workflow_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            let name = if name.is_empty() { "?" } else { name };
            format!(
                "Pending action #{id}: run workflow '{name}'",
                id = action.id
            )
        }
        _ => format!(
            "Pending action #{id}: {tool} with arguments {args}",
            id = action.id,
            tool = action.tool_name,
            args = crate::tools::pending_store::python_json_dumps(&action.arguments)
        ),
    };
    if !action.justification.is_empty() {
        summary.push_str(&format!("\n  Justification: {}", action.justification));
    }
    summary
}

/// Create a deterministic user reply for queued high-risk actions.
pub fn build_pending_action_reply(actions: &[PendingRecord]) -> String {
    let mut lines =
        vec!["I queued the following high-risk action(s) for confirmation:".to_string()];
    lines.extend(
        actions
            .iter()
            .map(|action| format!("- {}", format_pending_action(action))),
    );
    lines.push(
        "Reply with \"confirm <id>\" to run one, or \"cancel <id>\" to discard it.".to_string(),
    );
    lines.join("\n")
}
