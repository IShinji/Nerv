//! Local Ollama HTTP backend (reserved; dormant by default).
//!
//! Port of `_complete_ollama` in `brain/nerv/llm/client.py`, including the
//! ollama option profile from `brain/nerv/hardware.py` (thread cap on Apple
//! Silicon, RAM-scaled context window).

use std::env;
use std::time::Duration;

use serde_json::json;
use tracing::warn;

use super::{truncate, ChatMessage, CompletionResult, LlmError, DEFAULT_OLLAMA_URL};

pub async fn complete(
    messages: &[ChatMessage],
    model_name: &str,
    temperature: f64,
    json_mode: bool,
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
    Ok(CompletionResult { content })
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
fn total_ram_gb() -> u64 {
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
fn total_ram_gb() -> u64 {
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
fn total_ram_gb() -> u64 {
    warn!("Could not determine system RAM on this platform, assuming 8 GB");
    8
}

/// Port of `_is_apple_silicon` (`sysctl machdep.cpu.brand_string` contains
/// "Apple").
#[cfg(target_os = "macos")]
fn is_apple_silicon() -> bool {
    std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("Apple"))
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn is_apple_silicon() -> bool {
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
