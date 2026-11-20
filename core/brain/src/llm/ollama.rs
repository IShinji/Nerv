//! Local Ollama HTTP backend (reserved; dormant by default).
//!
//! Port of `_complete_ollama` in `brain/nerv/llm/client.py`, including the
//! ollama option profile from `brain/nerv/hardware.py` (thread cap on Apple
//! Silicon, RAM-scaled context window).

use std::env;
use std::time::Duration;

use serde_json::json;
use tracing::warn;

use super::{truncate, ChatMessage, CompletionResult, LlmError, ToolCall, DEFAULT_OLLAMA_URL};

pub async fn complete(
    messages: &[ChatMessage],
    model_name: &str,
    temperature: f64,
    json_mode: bool,
    tools: Option<&[serde_json::Value]>,
    timeout: Duration,
) -> Result<CompletionResult, LlmError> {
    let base_url = env::var("NERV_OLLAMA_URL").unwrap_or_else(|_| DEFAULT_OLLAMA_URL.to_string());

    let mut payload = json!({
        "model": model_name,
        "messages": messages,
        "stream": false,
        "options": ollama_options(temperature),
    });
    if json_mode {
        payload["format"] = json!("json");
    }
    if let Some(tools) = tools.filter(|t| !t.is_empty()) {
        payload["tools"] = json!(tools);
    }

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| LlmError::Ollama(e.to_string()))?;
    let response = client
        .post(format!("{base_url}/api/chat"))
        .json(&payload)
        .send()
        .await
        .map_err(|e| LlmError::Ollama(e.to_string()))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(LlmError::Ollama(format!(
            "HTTP {}: {}",
            status,
            truncate(&body, 500)
        )));
    }

    let data: serde_json::Value = response
        .json()
        .await
        .map_err(|e| LlmError::Ollama(e.to_string()))?;
    // Python: message_obj.get("content", "") or ""
    let content = data["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    // Python: list(message_obj.get("tool_calls", []) or []) — items keep the
    // {"function": {"name", "arguments"}} shape the loop expects.
    let tool_calls: Vec<ToolCall> = data["message"]["tool_calls"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|tc| serde_json::from_value(tc.clone()).ok())
        .collect();
    Ok(CompletionResult {
        content,
        tool_calls,
        runs_own_tool_loop: false,
        provider: String::new(),
        duration_ms: None,
        cost_usd: None,
        usage: None,
    })
}

/// ollama_options from `nerv/hardware.py`: limit threads on Apple Silicon to
/// avoid performance-core starvation, scale the context window with RAM.
fn ollama_options(temperature: f64) -> serde_json::Value {
    let ram_gb = total_ram_gb();
    let mut options = json!({ "temperature": temperature, "num_predict": 1024 });
    if is_apple_silicon() {
        options["num_thread"] = json!(if ram_gb >= 16 { 8 } else { 4 });
    }
    options["num_ctx"] = json!(if ram_gb >= 16 { 4096 } else { 2048 });
    options
}

/// Port of `_get_total_ram_gb` (macOS `sysctl hw.memsize`, Linux
/// `/proc/meminfo`, conservative 8 GB fallback).
#[cfg(target_os = "macos")]
pub fn total_ram_gb() -> u64 {
    if let Ok(output) = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
    {
        if let Ok(text) = String::from_utf8(output.stdout) {
            if let Ok(bytes) = text.trim().parse::<u64>() {
                return bytes / (1024 * 1024 * 1024);
            }
        }
    }
    warn!("Could not determine system RAM, assuming 8 GB");
    8
}

#[cfg(target_os = "linux")]
pub fn total_ram_gb() -> u64 {
    if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
        for line in meminfo.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                let kb = rest
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse::<u64>().ok());
                if let Some(kb) = kb {
                    return kb / (1024 * 1024);
                }
            }
        }
    }
    warn!("Could not determine system RAM, assuming 8 GB");
    8
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn total_ram_gb() -> u64 {
    warn!("Could not determine system RAM on this platform, assuming 8 GB");
    8
}

/// Port of `_is_apple_silicon` (`sysctl machdep.cpu.brand_string` contains
/// "Apple").
#[cfg(target_os = "macos")]
pub fn is_apple_silicon() -> bool {
    std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("Apple"))
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
pub fn is_apple_silicon() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_options_scale_with_ram() {
        // The shape (not the host-specific values) is pinned here: always
        // num_predict 1024 and num_ctx, num_thread only on Apple Silicon.
        let options = ollama_options(0.1);
        assert_eq!(options["temperature"], json!(0.1));
        assert_eq!(options["num_predict"], json!(1024));
        let ctx = options["num_ctx"].as_i64().unwrap();
        assert!(ctx == 2048 || ctx == 4096);
        let threads = options.get("num_thread");
        if is_apple_silicon() {
            let t = threads.unwrap().as_i64().unwrap();
            assert!(t == 4 || t == 8);
        } else {
            assert!(threads.is_none());
        }
    }
}

/// The hardware profile for the setup wizard (port of
/// `get_hardware_profile`): recommended router model scales with RAM.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HardwareProfile {
    pub os_name: String,
    pub arch: String,
    pub ram_gb: u64,
    pub has_apple_silicon: bool,
    pub recommended_router_model: String,
}

/// The router model recommendation from the old python table:
/// ≥32 GB → qwen2.5:3b, otherwise the conservative qwen2.5:1.5b.
pub fn recommended_router_model(ram_gb: u64) -> &'static str {
    if ram_gb >= 32 {
        "qwen2.5:3b"
    } else {
        "qwen2.5:1.5b"
    }
}

/// The full profile for the wizard UI.
pub fn hardware_profile() -> HardwareProfile {
    let ram_gb = total_ram_gb();
    HardwareProfile {
        os_name: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        ram_gb,
        has_apple_silicon: is_apple_silicon(),
        recommended_router_model: recommended_router_model(ram_gb).to_string(),
    }
}

/// The models installed in the local Ollama instance (`/api/tags`), or an
/// empty list when the server is unreachable.
pub async fn ollama_installed_models(base_url: &str) -> Vec<String> {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(client) => client,
        Err(_) => return Vec::new(),
    };
    let Ok(response) = client.get(format!("{base_url}/api/tags")).send().await else {
        return Vec::new();
    };
    let Ok(data) = response.json::<serde_json::Value>().await else {
        return Vec::new();
    };
    data.get("models")
        .and_then(|v| v.as_array())
        .map(|models| {
            models
                .iter()
                .filter_map(|m| {
                    m.get("name")
                        .or_else(|| m.get("model"))
                        .and_then(|n| n.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Pull a model through the Ollama HTTP API (blocking until done). Returns
/// whether it succeeded; the wizard shows progress through the CLI instead
/// when it needs a progress bar.
pub async fn ollama_pull(base_url: &str, model: &str) -> Result<(), String> {
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base_url}/api/pull"))
        .json(&serde_json::json!({"name": model}))
        .timeout(Duration::from_secs(1800))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "ollama pull failed with status {}",
            response.status()
        ))
    }
}
