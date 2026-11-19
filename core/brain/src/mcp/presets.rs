//! Builtin MCP server presets and action bindings.
//!
//! Port of `brain/nerv/mcp/presets.py`.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::client::{McpServerConfig, DEFAULT_MCP_REQUEST_TIMEOUT};

/// Map a logical Nerv action to a concrete MCP tool call.
#[derive(Debug, Clone)]
pub struct McpActionBinding {
    pub tool_name: &'static str,
    /// Build the MCP tool's request arguments from Nerv's action arguments.
    pub build_arguments: fn(&Value) -> Value,
}

/// Builtin preset for a known MCP server.
#[derive(Debug, Clone)]
pub struct McpServerPreset {
    pub name: &'static str,
    pub description: &'static str,
    pub source_url: &'static str,
    pub notes: &'static [&'static str],
    pub transport: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    pub env: &'static [(&'static str, &'static str)],
    pub cwd: &'static str,
    pub capabilities: &'static [&'static str],
    pub default_action_map: &'static [(&'static str, &'static str)],
    pub action_bindings: &'static [McpActionBinding],
}

fn passthrough_url(arguments: &Value) -> Value {
    json!({"url": arguments.get("url").and_then(|v| v.as_str()).unwrap_or("")})
}

fn empty_arguments(_arguments: &Value) -> Value {
    json!({})
}

fn evaluate_script_arguments(arguments: &Value) -> Value {
    // Wrap raw script text into Chrome DevTools MCP's function API.
    json!({
        "function": "(source) => { return globalThis.eval(source); }",
        "args": [arguments.get("script").and_then(|v| v.as_str()).unwrap_or("")],
    })
}

fn submit_prompt_arguments(_arguments: &Value) -> Value {
    // Submit the active prompt box with Enter.
    json!({"key": "Enter"})
}

fn wait_for_text_arguments(arguments: &Value) -> Value {
    // Convert Nerv polling arguments to Chrome DevTools MCP wait arguments.
    let timeout_secs = arguments
        .get("timeout_secs")
        .and_then(|v| v.as_i64())
        .unwrap_or(30)
        .max(1);
    json!({
        "text": [arguments.get("wait_for").and_then(|v| v.as_str()).unwrap_or("")],
        "timeout": timeout_secs * 1000,
    })
}

/// Builtin presets, in a stable (sorted by name) order.
pub static BUILTIN_MCP_PRESETS: &[McpServerPreset] = &[McpServerPreset {
    name: "chrome_devtools_official",
    description: "Official Chrome DevTools MCP server for interactive browser control.",
    source_url: "https://github.com/ChromeDevTools/chrome-devtools-mcp",
    notes: &[
        "Uses the official Chrome DevTools MCP package via npx.",
        "Best fit for browser.interactive capability in MCP-first mode.",
        "Supports richer actions than Nerv's current generic browser abstraction.",
    ],
    transport: "stdio",
    command: "npx",
    args: &["-y", "chrome-devtools-mcp@latest"],
    env: &[],
    cwd: "",
    capabilities: &["browser.interactive"],
    default_action_map: &[
        ("open_url", "new_page"),
        ("get_page_text", "take_snapshot"),
        ("run_javascript", "evaluate_script"),
        ("submit_prompt", "press_key"),
        ("wait_for_text", "wait_for"),
    ],
    action_bindings: &[
        McpActionBinding {
            tool_name: "new_page",
            build_arguments: passthrough_url,
        },
        McpActionBinding {
            tool_name: "take_snapshot",
            build_arguments: empty_arguments,
        },
        McpActionBinding {
            tool_name: "evaluate_script",
            build_arguments: evaluate_script_arguments,
        },
        McpActionBinding {
            tool_name: "press_key",
            build_arguments: submit_prompt_arguments,
        },
        McpActionBinding {
            tool_name: "wait_for",
            build_arguments: wait_for_text_arguments,
        },
    ],
}];

/// Look up a builtin MCP preset by name.
pub fn get_builtin_mcp_preset(name: &str) -> Option<&'static McpServerPreset> {
    let normalized = name.trim();
    if normalized.is_empty() {
        return None;
    }
    BUILTIN_MCP_PRESETS
        .iter()
        .find(|preset| preset.name == normalized)
}

/// All builtin presets in a stable order.
pub fn list_builtin_mcp_presets() -> Vec<&'static McpServerPreset> {
    let mut presets: Vec<&'static McpServerPreset> = BUILTIN_MCP_PRESETS.iter().collect();
    presets.sort_by_key(|preset| preset.name);
    presets
}

/// Render a copy-pasteable YAML config snippet for a preset.
pub fn render_mcp_preset_snippet(preset: &McpServerPreset, server_name: &str) -> String {
    format!(
        "mcp:\n  servers:\n    {server_name}:\n      enabled: true\n      preset: {}\n      transport: {}\n      command: {}\n      args:\n{}\n      capabilities:\n{}",
        preset.name,
        preset.transport,
        preset.command,
        preset
            .args
            .iter()
            .map(|a| format!("        - {a}"))
            .collect::<Vec<_>>()
            .join("\n"),
        preset
            .capabilities
            .iter()
            .map(|c| format!("        - {c}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Merge builtin preset defaults with explicit server config values.
pub fn merge_preset_into_server_config(name: &str, raw_value: &Value) -> McpServerConfig {
    let preset_name = raw_value
        .get("preset")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let preset = get_builtin_mcp_preset(&preset_name);

    let mut transport = raw_value
        .get("transport")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if transport.is_empty() {
        transport = preset
            .map(|p| p.transport.to_string())
            .unwrap_or_else(|| "stdio".to_string());
    }

    let mut command = raw_value
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if command.is_empty() {
        if let Some(p) = preset {
            command = p.command.to_string();
        }
    }

    let mut args: Vec<String> = raw_value
        .get("args")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if args.is_empty() {
        if let Some(p) = preset {
            args = p.args.iter().map(|s| s.to_string()).collect();
        }
    }

    let mut env: HashMap<String, String> = raw_value
        .get("env")
        .and_then(|v| v.as_object())
        .map(|o| o.iter().map(|(k, v)| (k.clone(), v.to_string())).collect())
        .unwrap_or_default();
    if env.is_empty() {
        if let Some(p) = preset {
            env = p
                .env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        }
    }

    let mut cwd = raw_value
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if cwd.is_empty() {
        if let Some(p) = preset {
            cwd = p.cwd.to_string();
        }
    }

    let mut capabilities: Vec<String> = raw_value
        .get("capabilities")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if capabilities.is_empty() {
        if let Some(p) = preset {
            capabilities = p.capabilities.iter().map(|s| s.to_string()).collect();
        }
    }

    let mut action_map: HashMap<String, String> = preset
        .map(|p| {
            p.default_action_map
                .iter()
                .map(|(a, t)| (a.to_string(), t.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if let Some(raw_map) = raw_value.get("action_map").and_then(|v| v.as_object()) {
        for (action, tool) in raw_map {
            if let Some(tool) = tool.as_str() {
                action_map.insert(action.clone(), tool.to_string());
            }
        }
    }

    McpServerConfig {
        name: name.to_string(),
        preset: preset_name,
        enabled: raw_value
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        transport,
        command,
        args,
        env,
        cwd,
        capabilities,
        action_map,
        request_timeout: parse_timeout(raw_value.get("request_timeout")),
    }
}

/// A positive number of seconds, else the default.
fn parse_timeout(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Number(n)) => {
            let secs = n.as_f64().unwrap_or(0.0);
            if secs > 0.0 {
                secs
            } else {
                DEFAULT_MCP_REQUEST_TIMEOUT
            }
        }
        _ => DEFAULT_MCP_REQUEST_TIMEOUT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_lookup_and_listing() {
        assert!(get_builtin_mcp_preset("chrome_devtools_official").is_some());
        assert!(get_builtin_mcp_preset("  ").is_none());
        assert!(get_builtin_mcp_preset("unknown").is_none());
        assert!(!list_builtin_mcp_presets().is_empty());
    }

    #[test]
    fn merge_uses_preset_defaults_and_explicit_overrides() {
        let raw = serde_json::json!({"enabled": true, "preset": "chrome_devtools_official"});
        let config = merge_preset_into_server_config("chrome", &raw);
        assert!(config.enabled);
        assert_eq!(config.command, "npx");
        assert_eq!(config.args, vec!["-y", "chrome-devtools-mcp@latest"]);
        assert_eq!(config.capabilities, vec!["browser.interactive"]);
        assert_eq!(
            config.action_map.get("open_url").map(String::as_str),
            Some("new_page")
        );

        let raw = serde_json::json!({"enabled": true, "command": "node", "args": ["srv.js"],
            "action_map": {"open_url": "my_open"}, "request_timeout": 45});
        let config = merge_preset_into_server_config("custom", &raw);
        assert_eq!(config.command, "node");
        assert!(config.action_map.contains_key("open_url"));
        assert_eq!(config.request_timeout, 45.0);
    }

    #[test]
    fn snippet_renders_yaml() {
        let preset = get_builtin_mcp_preset("chrome_devtools_official").unwrap();
        let snippet = render_mcp_preset_snippet(preset, "chrome");
        assert!(snippet.contains("preset: chrome_devtools_official"));
        assert!(snippet.contains("enabled: true"));
    }
}
