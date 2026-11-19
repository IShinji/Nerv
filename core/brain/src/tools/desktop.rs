//! Desktop and browser automation: Chrome control, screenshots, input events.
//!
//! Port of `brain/nerv/tools/desktop.py` — the outward-acting tools:
//! `desktop_control` and any path outside the workspace sandbox are suspended
//! for user confirmation before they run (see `super::sandbox`).
//!
//! `browser_interactive` prefers a configured MCP browser provider; the MCP
//! manager is still python, so that attempt goes through the
//! `browser_mcp_attempt` RPC and falls back to the native chrome control.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

use super::helpers::{ensure_url, resolve_tool_path, truncate};
use super::registry::{ToolContext, ToolDefinition, ToolResult};
use super::sandbox::scrubbed_env;

const DEFAULT_SCREENSHOT_NAME: &str = "nerv-screenshot";
const CHROME_APP_NAME: &str = "Google Chrome";

const SPECIAL_KEY_CODES: &[(&str, i64)] = &[
    ("enter", 36),
    ("return", 36),
    ("tab", 48),
    ("space", 49),
    ("escape", 53),
    ("esc", 53),
    ("left", 123),
    ("right", 124),
    ("down", 125),
    ("up", 126),
];

const APPLE_MODIFIERS: &[(&str, &str)] = &[
    ("command", "command down"),
    ("cmd", "command down"),
    ("shift", "shift down"),
    ("option", "option down"),
    ("alt", "option down"),
    ("control", "control down"),
    ("ctrl", "control down"),
];

async fn run_subprocess(args: &[String]) -> std::io::Result<(i32, String, String)> {
    let mut command = tokio::process::Command::new(&args[0]);
    command
        .args(&args[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .envs(scrubbed_env(&[]))
        .kill_on_drop(true);
    let output = command.output().await?;
    Ok((
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

async fn run_applescript(lines: &[String]) -> std::io::Result<(i32, String, String)> {
    let mut args = vec!["osascript".to_string()];
    for line in lines {
        args.push("-e".to_string());
        args.push(line.clone());
    }
    run_subprocess(&args).await
}

/// Escape text embedded into AppleScript string literals.
pub fn escape_applescript_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Convert a free-form modifiers string into AppleScript modifier flags.
pub fn normalize_modifiers(modifiers: &str) -> Vec<String> {
    let mut normalized = Vec::new();
    for item in modifiers.replace('+', ",").split(',') {
        let token = item.trim().to_lowercase();
        if token.is_empty() {
            continue;
        }
        if let Some(flag) = APPLE_MODIFIERS
            .iter()
            .find(|(name, _)| *name == token)
            .map(|(_, flag)| *flag)
        {
            normalized.push(flag.to_string());
        }
    }
    normalized
}

/// Resolve a screenshot output path, creating a temp path when omitted.
fn resolve_capture_path(output_path: &str, project_root: &Path) -> PathBuf {
    if !output_path.trim().is_empty() {
        let path = resolve_tool_path(output_path, project_root);
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/")));
        return path;
    }
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    std::env::temp_dir().join(format!("{DEFAULT_SCREENSHOT_NAME}-{timestamp}.png"))
}

/// Best-effort OCR for macOS screenshots using Vision via Swift.
async fn extract_text_from_image(image_path: &Path) -> String {
    if !cfg!(target_os = "macos") {
        return String::new();
    }

    let script = r#"
import AppKit
import Foundation
import Vision

let imagePath = CommandLine.arguments[1]
let url = URL(fileURLWithPath: imagePath)

guard
    let image = NSImage(contentsOf: url),
    let tiff = image.tiffRepresentation,
    let bitmap = NSBitmapImageRep(data: tiff),
    let cgImage = bitmap.cgImage
else {
    fputs("Failed to load image\n", stderr)
    exit(1)
}

let request = VNRecognizeTextRequest()
request.recognitionLevel = .accurate
request.usesLanguageCorrection = true

let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
do {
    try handler.perform([request])
    let observations = request.results as? [VNRecognizedTextObservation] ?? []
    for observation in observations {
        if let candidate = observation.topCandidates(1).first {
            print(candidate.string)
        }
    }
} catch {
    fputs("\(error)\n", stderr)
    exit(2)
}"#;

    let args = vec![
        "swift".to_string(),
        "-e".to_string(),
        script.to_string(),
        image_path.display().to_string(),
    ];
    match run_subprocess(&args).await {
        Ok((0, stdout, _)) => stdout.trim().to_string(),
        _ => String::new(),
    }
}

/// Build AppleScript for a key press or hotkey.
pub fn build_hotkey_applescript(key: &str, modifiers: &str) -> Result<String, String> {
    let normalized_key = key.trim().to_lowercase();
    let modifier_flags = normalize_modifiers(modifiers);
    let using_clause = if modifier_flags.is_empty() {
        String::new()
    } else {
        format!(" using {{{}}}", modifier_flags.join(", "))
    };

    if let Some((_, code)) = SPECIAL_KEY_CODES
        .iter()
        .find(|(name, _)| *name == normalized_key)
    {
        return Ok(format!(
            "tell application \"System Events\" to key code {code}{using_clause}"
        ));
    }

    if key.chars().count() == 1 {
        let escaped_key = escape_applescript_text(key);
        return Ok(format!(
            "tell application \"System Events\" to keystroke \"{escaped_key}\"{using_clause}"
        ));
    }

    Err(
        "Unsupported key name. Use a single character or a named key like \
         enter, tab, escape, up, down, left, or right."
            .to_string(),
    )
}

/// Module-level flag to avoid repeated subprocess calls (python global).
fn chrome_as_enabled() -> &'static std::sync::atomic::AtomicBool {
    static FLAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    &FLAG
}

/// Ensure Chrome allows JavaScript execution via AppleScript.
///
/// The 'Allow JavaScript from Apple Events' toggle can only be changed via
/// the Chrome UI menu. Probe first; if blocked, try to toggle the menu via UI
/// scripting; if that fails (no Accessibility permission), show a clear
/// notification.
async fn ensure_chrome_applescript_enabled() {
    use std::sync::atomic::Ordering;
    if chrome_as_enabled().load(Ordering::Relaxed) {
        return;
    }

    let probe = run_applescript(&[
        format!("tell application \"{CHROME_APP_NAME}\""),
        "if (count of windows) = 0 then".to_string(),
        "  make new window".to_string(),
        "  delay 1".to_string(),
        "end if".to_string(),
        "try".to_string(),
        "  return execute active tab of front window javascript \"1+1\"".to_string(),
        "on error".to_string(),
        "  return \"BLOCKED\"".to_string(),
        "end try".to_string(),
        "end tell".to_string(),
    ])
    .await;

    if let Ok((0, stdout, _)) = probe {
        if stdout.trim() != "BLOCKED" {
            chrome_as_enabled().store(true, Ordering::Relaxed);
            return;
        }
    }

    tracing::info!("Chrome AppleScript JS is blocked. Attempting to enable via menu toggle...");
    let toggle = run_applescript(&[
        format!("tell application \"{CHROME_APP_NAME}\" to activate"),
        "delay 0.5".to_string(),
        "tell application \"System Events\"".to_string(),
        format!("  tell process \"{CHROME_APP_NAME}\""),
        "    click menu item \"Allow JavaScript from Apple Events\" of menu \"Developer\" of menu item \"Developer\" of menu \"View\" of menu bar 1".to_string(),
        "  end tell".to_string(),
        "end tell".to_string(),
        "delay 1".to_string(),
    ])
    .await;

    if matches!(toggle, Ok((0, _, _))) {
        tracing::info!("Successfully toggled Chrome JS permission via menu.");
        chrome_as_enabled().store(true, Ordering::Relaxed);
        return;
    }

    tracing::warn!(
        "Could not auto-enable Chrome JS permission. Please enable it \
         manually: Chrome menu → View → Developer → Allow JavaScript from \
         Apple Events"
    );
    let _ = run_subprocess(&[
        "osascript".to_string(),
        "-e".to_string(),
        "display notification \"Please enable: Chrome → View → Developer → Allow JavaScript from Apple Events\" with title \"Nerv Setup Required\"".to_string(),
    ])
    .await;
}

/// Validate that Chrome automation is available on the current host.
async fn ensure_chrome_running() -> Option<ToolResult> {
    if !cfg!(target_os = "macos") {
        return Some(ToolResult::error(format!(
            "chrome_browser MVP is currently implemented for macOS only. \
             Current platform: {}",
            std::env::consts::OS
        )));
    }

    let chrome_path = PathBuf::from("/Applications").join(format!("{CHROME_APP_NAME}.app"));
    if !chrome_path.exists() {
        return Some(ToolResult::error(format!(
            "{CHROME_APP_NAME} is not installed in /Applications."
        )));
    }

    ensure_chrome_applescript_enabled().await;
    None
}

/// Execute AppleScript against Google Chrome and return stdout.
async fn chrome_run_script(lines: &[String]) -> ToolResult {
    if let Some(precheck) = ensure_chrome_running().await {
        return precheck;
    }

    match run_applescript(lines).await {
        Ok((code, stdout, stderr)) => {
            if code != 0 {
                let message = if !stderr.trim().is_empty() {
                    stderr.trim().to_string()
                } else if !stdout.trim().is_empty() {
                    stdout.trim().to_string()
                } else {
                    "unknown error".to_string()
                };
                ToolResult::error(format!("Chrome automation failed: {message}"))
            } else {
                ToolResult::ok(stdout.trim().to_string())
            }
        }
        Err(e) => ToolResult::error(format!("Chrome automation failed: {e}")),
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ChromeState {
    title: String,
    url: String,
    loading: bool,
}

/// Read basic state from the active Chrome tab.
async fn chrome_get_state() -> ToolResult {
    let result = chrome_run_script(&[
        format!("tell application \"{CHROME_APP_NAME}\""),
        "activate".to_string(),
        "if (count of windows) = 0 then return \"NO_WINDOW\"".to_string(),
        "set tab_title to title of active tab of front window".to_string(),
        "set tab_url to URL of active tab of front window".to_string(),
        "set tab_loading to (loading of active tab of front window as text)".to_string(),
        "return tab_title & linefeed & tab_url & linefeed & tab_loading".to_string(),
        "end tell".to_string(),
    ])
    .await;
    if result.is_error {
        return result;
    }

    if result.content == "NO_WINDOW" {
        return ToolResult::error("Chrome has no open window.");
    }

    let mut lines = result.content.lines();
    let state = ChromeState {
        title: lines.next().unwrap_or("").to_string(),
        url: lines.next().unwrap_or("").to_string(),
        loading: lines.next().unwrap_or("").to_lowercase() == "true",
    };
    ToolResult::ok(serde_json::to_string(&state).unwrap_or_default())
}

/// Run JavaScript inside the active Chrome tab.
async fn chrome_execute_javascript(script: &str) -> ToolResult {
    let escaped_script = escape_applescript_text(script);
    let result = chrome_run_script(&[
        format!("tell application \"{CHROME_APP_NAME}\""),
        "activate".to_string(),
        "if (count of windows) = 0 then return \"NO_WINDOW\"".to_string(),
        format!("return execute active tab of front window javascript \"{escaped_script}\""),
        "end tell".to_string(),
    ])
    .await;
    if result.is_error {
        return result;
    }
    if result.content == "NO_WINDOW" {
        return ToolResult::error("Chrome has no open window.");
    }
    result
}

/// Extract readable page text from the active tab.
async fn chrome_page_text(max_chars: usize) -> ToolResult {
    let script = r#"(() => {
  const text = document.body ? document.body.innerText : "";
  return text || "";
})()"#;
    let result = chrome_execute_javascript(script).await;
    if result.is_error {
        return result;
    }
    ToolResult::ok(truncate(
        result.content.trim(),
        max_chars,
        "chrome_page_text",
    ))
}

/// Validate and normalize browser action arguments. `Err` carries the
/// validation error reply.
#[allow(clippy::too_many_arguments)]
fn build_browser_arguments(
    action: &str,
    url: &str,
    text: &str,
    script: &str,
    wait_for: &str,
    timeout_secs: i64,
    poll_interval_secs: f64,
    max_chars: i64,
) -> Result<Value, ToolResult> {
    let normalized = action.trim().to_lowercase();

    match normalized.as_str() {
        "open_url" => {
            let target_url = ensure_url(url);
            if target_url.is_empty() {
                return Err(ToolResult::error("open_url requires `url`."));
            }
            Ok(json!({"url": target_url}))
        }
        "get_state" => Ok(json!({})),
        "get_page_text" => Ok(json!({"max_chars": max_chars})),
        "run_javascript" => {
            if script.trim().is_empty() {
                Err(ToolResult::error("run_javascript requires `script`."))
            } else {
                Ok(json!({"script": script}))
            }
        }
        "fill_prompt" => {
            if text.is_empty() {
                Err(ToolResult::error("fill_prompt requires `text`."))
            } else {
                Ok(json!({"text": text}))
            }
        }
        "submit_prompt" => Ok(json!({})),
        "wait_for_idle" => Ok(json!({
            "timeout_secs": timeout_secs.max(1),
            "poll_interval_secs": poll_interval_secs.max(0.1),
        })),
        "wait_for_text" => {
            if wait_for.trim().is_empty() {
                Err(ToolResult::error("wait_for_text requires `wait_for`."))
            } else {
                Ok(json!({
                    "wait_for": wait_for,
                    "timeout_secs": timeout_secs.max(1),
                    "poll_interval_secs": poll_interval_secs.max(0.1),
                    "max_chars": max_chars,
                }))
            }
        }
        _ => Err(ToolResult::error(
            "Unsupported browser_interactive action. Use one of: open_url, \
             get_state, get_page_text, fill_prompt, submit_prompt, \
             wait_for_text, wait_for_idle, run_javascript.",
        )),
    }
}

pub fn browser_interactive() -> ToolDefinition {
    ToolDefinition {
        name: "browser_interactive",
        description: "Interactive browser capability. Prefers a configured MCP browser provider and falls back to local chrome_browser when MCP is unavailable. Supported actions: open_url, get_state, get_page_text, fill_prompt, submit_prompt, wait_for_text, wait_for_idle, run_javascript.",
        requires_confirmation: true,
        parameters: json!({
            "type": "object",
            "properties": {
                "action": {"type": "string"},
                "url": {"type": "string"},
                "text": {"type": "string"},
                "script": {"type": "string"},
                "wait_for": {"type": "string"},
                "timeout_secs": {"type": "integer"},
                "poll_interval_secs": {"type": "number"},
                "max_chars": {"type": "integer"}
            },
            "required": ["action"]
        }),
        func: |ctx, args| {
            let ctx = ToolContext {
                project_root: ctx.project_root.clone(),
                workspace_root: ctx.workspace_root.clone(),
                scope: ctx.scope.clone(),
                brain: ctx.brain.clone(),
            confirmed: None,
        };
            let args = args.clone();
            Box::pin(async move { browser_interactive_impl(&ctx, &args).await })
        },
    }
}

async fn browser_interactive_impl(ctx: &ToolContext, args: &Value) -> ToolResult {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let (arguments, validation_error) = normalize_arguments(args);
    if let Some(error) = validation_error {
        return error;
    }
    let arguments = arguments.unwrap_or_else(|| json!({}));
    let normalized = action.trim().to_lowercase();

    // MCP-first: the MCP manager is still python; give it the first shot via
    // the RPC and fall back to the native chrome control when unavailable.
    if let Some(brain) = ctx.brain.as_ref() {
        if let Ok(attempt) = brain
            .call(
                "browser_mcp_attempt",
                json!({
                    "action": normalized,
                    "arguments": arguments,
                }),
            )
            .await
        {
            // python returns null when no MCP browser provider handled it.
            if !attempt.is_null() {
                return ToolResult {
                    content: attempt["content"].as_str().unwrap_or("").to_string(),
                    is_error: attempt["is_error"].as_bool().unwrap_or(false),
                    is_pending: false,
                };
            }
        }
    }

    chrome_browser_impl(ctx, &normalized, &arguments).await
}

#[allow(clippy::too_many_arguments)]
pub fn normalize_arguments(args: &Value) -> (Option<Value>, Option<ToolResult>) {
    let get = |key: &str| {
        args.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let get_i = |key: &str, default: i64| args.get(key).and_then(|v| v.as_i64()).unwrap_or(default);
    let get_f = |key: &str, default: f64| args.get(key).and_then(|v| v.as_f64()).unwrap_or(default);

    let action = get("action");
    match build_browser_arguments(
        &action,
        &get("url"),
        &get("text"),
        &get("script"),
        &get("wait_for"),
        get_i("timeout_secs", 30),
        get_f("poll_interval_secs", 1.0),
        get_i("max_chars", 4000),
    ) {
        Ok(arguments) => (Some(arguments), None),
        Err(error) => (None, Some(error)),
    }
}

pub fn chrome_browser() -> ToolDefinition {
    ToolDefinition {
        name: "chrome_browser",
        description: "Automate Google Chrome for multi-step browser tasks. Supported actions: open_url, get_state, get_page_text, fill_prompt, submit_prompt, wait_for_text, wait_for_idle, run_javascript.",
        requires_confirmation: true,
        parameters: json!({
            "type": "object",
            "properties": {
                "action": {"type": "string"},
                "url": {"type": "string"},
                "text": {"type": "string"},
                "script": {"type": "string"},
                "wait_for": {"type": "string"},
                "timeout_secs": {"type": "integer"},
                "poll_interval_secs": {"type": "number"},
                "max_chars": {"type": "integer"}
            },
            "required": ["action"]
        }),
        func: |_ctx, args| {
            let args = args.clone();
            Box::pin(async move {
                let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let (arguments, validation_error) = normalize_arguments(&args);
                if let Some(error) = validation_error {
                    return error;
                }
                let arguments = arguments.unwrap_or_else(|| json!({}));
                let normalized = action.trim().to_lowercase();
                chrome_browser_impl(
                    &ToolContext {
                        project_root: PathBuf::new(),
                        workspace_root: PathBuf::new(),
                        scope: String::new(),
                        brain: None,
            confirmed: None,
        },
                    &normalized,
                    &arguments,
                )
                .await
            })
        },
    }
}

async fn chrome_browser_impl(
    _ctx: &ToolContext,
    normalized: &str,
    arguments: &Value,
) -> ToolResult {
    let get_s = |key: &str| {
        arguments
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let get_i = |key: &str, default: i64| {
        arguments
            .get(key)
            .and_then(|v| v.as_i64())
            .unwrap_or(default)
    };
    let get_f = |key: &str, default: f64| {
        arguments
            .get(key)
            .and_then(|v| v.as_f64())
            .unwrap_or(default)
    };

    match normalized {
        "open_url" => {
            let url = get_s("url");
            chrome_run_script(&[
                format!("tell application \"{CHROME_APP_NAME}\""),
                "activate".to_string(),
                "if (count of windows) = 0 then make new window".to_string(),
                format!(
                    "set URL of active tab of front window to \"{}\"",
                    escape_applescript_text(&url)
                ),
                format!("return \"Opened URL: {url}\""),
                "end tell".to_string(),
            ])
            .await
        }
        "get_state" => {
            let state = chrome_get_state().await;
            if state.is_error {
                return state;
            }
            let payload: ChromeState =
                serde_json::from_str(&state.content).unwrap_or(ChromeState {
                    title: String::new(),
                    url: String::new(),
                    loading: false,
                });
            ToolResult::ok(format!(
                "Title: {}\nURL: {}\nLoading: {}",
                payload.title, payload.url, payload.loading
            ))
        }
        "get_page_text" => chrome_page_text(get_i("max_chars", 4000) as usize).await,
        "run_javascript" => chrome_execute_javascript(&get_s("script")).await,
        "fill_prompt" => {
            let text = get_s("text");
            let value = crate::tools::pending_store::python_json_dumps(&json!(text));
            let js = format!(
                r#"(() => {{
  const value = {value};
  const selectors = [
    'textarea',
    'div[contenteditable="true"]',
    '[contenteditable="true"]',
    'input[type="text"]',
    'input:not([type])'
  ];
  const isVisible = (el) => {{
    if (!el) return false;
    const rect = el.getBoundingClientRect();
    const style = window.getComputedStyle(el);
    return rect.width > 0 && rect.height > 0 &&
      style.visibility !== 'hidden' &&
      style.display !== 'none';
  }};

  for (const selector of selectors) {{
    const candidates = Array.from(document.querySelectorAll(selector));
    const target = candidates.find(isVisible);
    if (!target) continue;

    target.focus();
    if (target.isContentEditable) {{
      target.innerText = value;
      target.dispatchEvent(new InputEvent('input', {{ bubbles: true, data: value }}));
    }} else {{
      target.value = value;
      target.dispatchEvent(new Event('input', {{ bubbles: true }}));
      target.dispatchEvent(new Event('change', {{ bubbles: true }}));
    }}
    return `filled:${{selector}}`;
  }}

  return "NO_EDITABLE_INPUT";
}})()"#
            );
            let result = chrome_execute_javascript(&js).await;
            if result.is_error {
                return result;
            }
            if result.content == "NO_EDITABLE_INPUT" {
                return ToolResult::error(
                    "No editable prompt input was found on the current page.",
                );
            }
            ToolResult::ok(format!("Filled prompt using {}.", result.content))
        }
        "submit_prompt" => {
            let js = r#"(() => {
  const selectors = [
    'button[aria-label*="Send"]',
    'button[aria-label*="send"]',
    'button[aria-label*="Submit"]',
    'button[aria-label*="submit"]',
    'button[data-testid*="send"]',
    'button[title*="Send"]',
    'button[title*="send"]'
  ];
  const isVisible = (el) => {
    if (!el) return false;
    const rect = el.getBoundingClientRect();
    const style = window.getComputedStyle(el);
    return rect.width > 0 && rect.height > 0 &&
      style.visibility !== 'hidden' &&
      style.display !== 'none' &&
      !el.disabled;
  };

  for (const selector of selectors) {
    const target = Array.from(document.querySelectorAll(selector)).find(isVisible);
    if (target) {
      target.click();
      return `clicked:${selector}`;
    }
  }

  const active = document.activeElement;
  if (active) {
    const options = { key: 'Enter', code: 'Enter', keyCode: 13, which: 13, bubbles: true };
    active.dispatchEvent(new KeyboardEvent('keydown', options));
    active.dispatchEvent(new KeyboardEvent('keypress', options));
    active.dispatchEvent(new KeyboardEvent('keyup', options));
    return 'pressed:Enter';
  }

  return 'NO_SUBMIT_CONTROL';
})()"#;
            let result = chrome_execute_javascript(js).await;
            if result.is_error {
                return result;
            }
            if result.content == "NO_SUBMIT_CONTROL" {
                return ToolResult::error("No submit control was found on the current page.");
            }
            ToolResult::ok(format!("Submitted prompt using {}.", result.content))
        }
        "wait_for_idle" => {
            let deadline =
                std::time::Instant::now() + Duration::from_secs(get_i("timeout_secs", 30) as u64);
            let poll = Duration::from_secs_f64(get_f("poll_interval_secs", 1.0));
            while std::time::Instant::now() < deadline {
                let state = chrome_get_state().await;
                if state.is_error {
                    return state;
                }
                let payload: ChromeState =
                    serde_json::from_str(&state.content).unwrap_or(ChromeState {
                        title: String::new(),
                        url: String::new(),
                        loading: true,
                    });
                if !payload.loading {
                    return ToolResult::ok("Chrome tab is idle.");
                }
                tokio::time::sleep(poll).await;
            }
            ToolResult::error("Timed out waiting for the Chrome tab to finish loading.")
        }
        "wait_for_text" => {
            let wait_for = get_s("wait_for");
            let deadline =
                std::time::Instant::now() + Duration::from_secs(get_i("timeout_secs", 30) as u64);
            let poll = Duration::from_secs_f64(get_f("poll_interval_secs", 1.0));
            let max_chars = get_i("max_chars", 4000);
            while std::time::Instant::now() < deadline {
                let page_text = chrome_page_text(max_chars.max(12000) as usize).await;
                if page_text.is_error {
                    return page_text;
                }
                if page_text.content.contains(&wait_for) {
                    let excerpt = truncate(
                        &page_text.content,
                        max_chars as usize,
                        "chrome_wait_for_text",
                    );
                    return ToolResult::ok(format!(
                        "Observed target text in Chrome page:\n{excerpt}"
                    ));
                }
                tokio::time::sleep(poll).await;
            }
            ToolResult::error(format!(
                "Timed out waiting for text in Chrome page: {wait_for}"
            ))
        }
        _ => ToolResult::error(
            "Unsupported chrome_browser action. Use one of: open_url, \
             get_state, get_page_text, fill_prompt, submit_prompt, \
             wait_for_text, wait_for_idle, run_javascript.",
        ),
    }
}

pub fn screenshot() -> ToolDefinition {
    ToolDefinition {
        name: "screenshot",
        description: "Capture a screenshot of the current desktop and optionally return OCR text. This is privacy-sensitive and requires confirmation.",
        requires_confirmation: true,
        parameters: json!({
            "type": "object",
            "properties": {
                "output_path": {"type": "string"},
                "ocr": {"type": "boolean"}
            },
            "required": []
        }),
        func: |ctx, args| {
            let project_root = ctx.project_root.clone();
            let output_path = args
                .get("output_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let ocr = args.get("ocr").and_then(|v| v.as_bool()).unwrap_or(true);
            Box::pin(async move { screenshot_impl(&project_root, &output_path, ocr).await })
        },
    }
}

async fn screenshot_impl(project_root: &Path, output_path: &str, ocr: bool) -> ToolResult {
    let target_path = resolve_capture_path(output_path, project_root);

    let args: Vec<String> = if cfg!(target_os = "macos") {
        vec![
            "screencapture".to_string(),
            "-x".to_string(),
            target_path.display().to_string(),
        ]
    } else if cfg!(target_os = "linux") {
        if which("gnome-screenshot") {
            vec![
                "gnome-screenshot".to_string(),
                "--file".to_string(),
                target_path.display().to_string(),
            ]
        } else if which("scrot") {
            vec!["scrot".to_string(), target_path.display().to_string()]
        } else {
            return ToolResult::error(
                "Screenshot is unsupported on this Linux system (need gnome-screenshot or scrot).",
            );
        }
    } else {
        return ToolResult::error(format!(
            "Screenshot is not supported on platform: {}",
            std::env::consts::OS
        ));
    };

    match run_subprocess(&args).await {
        Ok((0, _, _)) => {}
        Ok((_, stdout, stderr)) => {
            let message = if !stderr.trim().is_empty() {
                stderr.trim().to_string()
            } else if !stdout.trim().is_empty() {
                stdout.trim().to_string()
            } else {
                "unknown error".to_string()
            };
            return ToolResult::error(format!("Failed to capture screenshot: {message}"));
        }
        Err(e) => return ToolResult::error(format!("Failed to capture screenshot: {e}")),
    }

    let mut lines = vec![format!("Saved screenshot to {}.", target_path.display())];
    if ocr {
        let extracted = extract_text_from_image(&target_path).await;
        if extracted.is_empty() {
            lines.push("OCR text was unavailable on this system.".to_string());
        } else {
            lines.push("OCR text:".to_string());
            lines.push(truncate(&extracted, 2500, "screenshot_ocr"));
        }
    }
    ToolResult::ok(lines.join("\n"))
}

fn which(binary: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                dir.join(format!("{binary}{}", std::env::consts::EXE_SUFFIX))
                    .exists()
                    || dir.join(binary).exists()
            })
        })
        .unwrap_or(false)
}

pub fn desktop_control() -> ToolDefinition {
    ToolDefinition {
        name: "desktop_control",
        description: "Perform explicit local desktop actions like opening apps, typing text, sending key presses, or opening URLs. This is high risk and requires confirmation.",
        requires_confirmation: true,
        parameters: json!({
            "type": "object",
            "properties": {
                "action": {"type": "string"},
                "application": {"type": "string"},
                "text": {"type": "string"},
                "key": {"type": "string"},
                "modifiers": {"type": "string"},
                "url": {"type": "string"}
            },
            "required": ["action"]
        }),
        func: |_ctx, args| {
            let args = args.clone();
            Box::pin(async move { desktop_control_impl(&args).await })
        },
    }
}

pub async fn desktop_control_impl(args: &Value) -> ToolResult {
    if !cfg!(target_os = "macos") {
        return ToolResult::error(format!(
            "desktop_control MVP is currently implemented for macOS only. \
             Current platform: {}",
            std::env::consts::OS
        ));
    }

    let normalized = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let application = args
        .get("application")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let modifiers = args.get("modifiers").and_then(|v| v.as_str()).unwrap_or("");
    let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");

    let arg_error =
        |message: &str| ToolResult::error(format!("desktop_control argument error: {message}"));

    let (run_args, summary): (Vec<String>, String) = match normalized.as_str() {
        "open_application" => {
            if application.trim().is_empty() {
                return arg_error("open_application requires `application`.");
            }
            (
                vec![
                    "open".to_string(),
                    "-a".to_string(),
                    application.to_string(),
                ],
                format!("Opened application: {application}"),
            )
        }
        "activate_application" => {
            if application.trim().is_empty() {
                return arg_error("activate_application requires `application`.");
            }
            let script = format!(
                "tell application \"{}\" to activate",
                escape_applescript_text(application)
            );
            (
                vec!["osascript".to_string(), "-e".to_string(), script],
                format!("Activated application: {application}"),
            )
        }
        "open_url" => {
            let mut target_url = url.trim().to_string();
            if target_url.is_empty() {
                return arg_error("open_url requires `url`.");
            }
            if !target_url.contains("://") {
                target_url = format!("https://{target_url}");
            }
            (
                vec!["open".to_string(), target_url.clone()],
                format!("Opened URL: {target_url}"),
            )
        }
        "type_text" => {
            if text.is_empty() {
                return arg_error("type_text requires `text`.");
            }
            let script = format!(
                "tell application \"System Events\" to keystroke \"{}\"",
                escape_applescript_text(text)
            );
            (
                vec!["osascript".to_string(), "-e".to_string(), script],
                format!("Typed text ({} chars)", text.chars().count()),
            )
        }
        "press_key" => {
            if key.trim().is_empty() {
                return arg_error("press_key requires `key`.");
            }
            let script = build_hotkey_applescript(key, "").unwrap_or_default();
            (
                vec!["osascript".to_string(), "-e".to_string(), script],
                format!("Pressed key: {key}"),
            )
        }
        "hotkey" => {
            if key.trim().is_empty() {
                return arg_error("hotkey requires `key`.");
            }
            let script = build_hotkey_applescript(key, modifiers).unwrap_or_default();
            (
                vec!["osascript".to_string(), "-e".to_string(), script],
                format!(
                    "Pressed hotkey: {} + {}",
                    if modifiers.is_empty() {
                        "no modifiers"
                    } else {
                        modifiers
                    },
                    key
                ),
            )
        }
        "quit_application" => {
            if application.trim().is_empty() {
                return arg_error("quit_application requires `application`.");
            }
            let script = format!(
                "tell application \"{}\" to quit",
                escape_applescript_text(application)
            );
            (
                vec!["osascript".to_string(), "-e".to_string(), script],
                format!("Quit application: {application}"),
            )
        }
        _ => {
            return ToolResult::error(
                "Unsupported desktop_control action. Use one of: \
                 open_application, activate_application, open_url, type_text, \
                 press_key, hotkey, quit_application.",
            )
        }
    };

    match run_subprocess(&run_args).await {
        Ok((0, stdout, _)) => {
            let details = stdout.trim().to_string();
            if details.is_empty() {
                ToolResult::ok(summary)
            } else {
                ToolResult::ok(format!(
                    "{summary}\n{}",
                    truncate(&details, 1500, "desktop_control")
                ))
            }
        }
        Ok((_, stdout, stderr)) => {
            let message = if !stderr.trim().is_empty() {
                stderr.trim().to_string()
            } else if !stdout.trim().is_empty() {
                stdout.trim().to_string()
            } else {
                "unknown error".to_string()
            };
            ToolResult::error(format!("desktop_control failed: {message}"))
        }
        Err(e) => ToolResult::error(format!("desktop_control failed: {e}")),
    }
}

use std::path::Path;
