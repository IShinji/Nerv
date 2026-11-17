//! System tools: the clock and shell access. Port of
//! `brain/nerv/tools/system.py`. `delegate_task` stays python until the
//! orchestrator's agent flows migrate (it recurses into the dispatch flow).

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use serde_json::{json, Value};

use super::helpers::{timezone_alias, truncate};
use super::registry::{ToolContext, ToolDefinition, ToolResult};
use super::sandbox::scrubbed_env;

const SHELL_TIMEOUT_SECONDS: u64 = 120;

pub fn current_time() -> ToolDefinition {
    ToolDefinition {
        name: "current_time",
        description:
            "Get the current date and time. Use an IANA timezone like America/Los_Angeles or a common city like Seattle. Use this instead of guessing current time/date.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "timezone": {"type": "string"}
            },
            "required": []
        }),
        func: |ctx, args| Box::pin(current_time_impl(ctx, args)),
    }
}

async fn current_time_impl(_ctx: &ToolContext, args: &Value) -> ToolResult {
    let requested = args
        .get("timezone")
        .and_then(|v| v.as_str())
        .unwrap_or("local")
        .trim()
        .to_string();

    let now = chrono::Local::now();
    if requested.is_empty() || requested.eq_ignore_ascii_case("local") {
        return ToolResult::ok(format!(
            "Current time in {}: {}",
            now.format("%Z"),
            now.format("%Y-%m-%d %H:%M:%S %Z")
        ));
    }

    let zone_name = timezone_alias(&requested.to_lowercase())
        .unwrap_or(&requested)
        .to_string();
    match chrono_tz::Tz::from_str(&zone_name) {
        Ok(tz) => {
            let local = now.with_timezone(&tz);
            ToolResult::ok(format!(
                "Current time in {zone_name}: {}",
                local.format("%Y-%m-%d %H:%M:%S %Z")
            ))
        }
        Err(_) => ToolResult::error(format!(
            "Unknown timezone '{requested}'. Please use an IANA timezone like \
             America/Los_Angeles."
        )),
    }
}

pub fn shell() -> ToolDefinition {
    ToolDefinition {
        name: "shell",
        description: "Execute a shell command on the host OS. This includes running build scripts, python commands, git, etc.",
        // Shell is always dangerous
        requires_confirmation: true,
        parameters: json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"}
            },
            "required": ["command"]
        }),
        func: |ctx, args| Box::pin(shell_impl(ctx, args)),
    }
}

async fn shell_impl(ctx: &ToolContext, args: &Value) -> ToolResult {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    // Children must not inherit provider keys the brain loaded from
    // personal/api_keys.env; only names the user allow-listed pass through.
    let passthrough = shell_env_passthrough(&ctx.project_root);
    let env = scrubbed_env(&passthrough);

    let mut shell_command = tokio::process::Command::new("/bin/sh");
    shell_command
        .arg("-c")
        .arg(&command)
        .envs(env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let output = tokio::time::timeout(
        Duration::from_secs(SHELL_TIMEOUT_SECONDS),
        shell_command.output(),
    )
    .await;

    let output = match output {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return ToolResult::error(format!("Shell execution failed: {e}")),
        Err(_) => {
            return ToolResult::error(format!(
                "Shell command timed out after {SHELL_TIMEOUT_SECONDS}s."
            ))
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut out = stdout.to_string();
    if !stderr.trim().is_empty() {
        out.push_str(&format!("\n[stderr]:\n{stderr}"));
    }

    ToolResult {
        content: format!(
            "Command executed. Exit code: {}\nOutput:\n{}",
            output.status.code().unwrap_or(-1),
            truncate(&out, super::helpers::MAX_TOOL_OUTPUT_CHARS, "shell")
        ),
        is_error: !output.status.success(),
        is_pending: false,
    }
}

fn shell_env_passthrough(project_root: &Path) -> Vec<String> {
    // Port of `get_shell_env_passthrough` — reads the merged config the same
    // way the loader does.
    nerv_shared::config::NervConfig::load(project_root)
        .map(|config| config.security.shell_env_passthrough)
        .unwrap_or_default()
}
