//! Cloud model backends — the Rust replacement for the litellm python
//! dependency (PRD v2: the `litellm:` provider prefix).
//!
//! Four vendor families are served directly, resolved from the model string
//! exactly like `parse_model` does (`vendor/model` prefix or bare name):
//!
//! - **anthropic** (`claude-*`, `anthropic/*`) — the Messages API
//! - **openai / o1** (`gpt-*`, `o1-*`, `openai/*`) — OpenAI chat completions
//! - **deepseek** (`deepseek/*`) — the OpenAI-compatible DeepSeek endpoint
//! - **gemini** (`gemini/*`) — the GenerateContent API
//!
//! API keys come from the same environment variables the availability probe
//! checks (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `DEEPSEEK_API_KEY`,
//! `GEMINI_API_KEY`).

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tracing::debug;

use super::{CompletionResult, FunctionCall, LlmError, ToolCall};
use crate::llm::ChatMessage;

/// Which vendor family a model string belongs to.
fn vendor(model_name: &str) -> &'static str {
    let lower = model_name.to_lowercase();
    if let Some((vendor, _)) = model_name.split_once('/') {
        return match vendor.to_lowercase().as_str() {
            "anthropic" => "anthropic",
            "openai" => "openai",
            "deepseek" => "deepseek",
            "gemini" | "google" => "gemini",
            _ => "openai", // assume OpenAI-compatible by default
        };
    }
    if lower.starts_with("claude") {
        return "anthropic";
    }
    if lower.starts_with("deepseek") {
        return "deepseek";
    }
    if lower.starts_with("gemini") {
        return "gemini";
    }
    "openai"
}

fn api_key(vendor: &str) -> Result<String, LlmError> {
    let var = match vendor {
        "anthropic" => "ANTHROPIC_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "gemini" => "GEMINI_API_KEY",
        _ => "OPENAI_API_KEY",
    };
    std::env::var(var)
        .ok()
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            LlmError::Unsupported(format!(
                "the {vendor} backend needs {var} in the environment (or personal/api_keys.env)"
            ))
        })
}

/// Load `personal/api_keys.env` once, if present. Python loaded the user's
/// cloud credentials this way before every cloud call; same contract, same
/// file, values never override what the environment already set.
fn load_api_keys_env(project_root: &Path) {
    static LOADED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    LOADED.get_or_init(|| {
        let path = project_root.join("personal").join("api_keys.env");
        let Ok(body) = std::fs::read_to_string(&path) else {
            return;
        };
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim().trim_matches('"').trim_matches('\'');
                if std::env::var_os(key).is_none() {
                    std::env::set_var(key, value);
                }
            }
        }
        debug!("loaded cloud API keys from {}", path.display());
    });
}

/// Run one assistant turn against the model's vendor API.
pub async fn complete(
    messages: &[ChatMessage],
    model_name: &str,
    tools: Option<&[Value]>,
    temperature: f64,
    json_mode: bool,
    timeout: Duration,
) -> Result<CompletionResult, LlmError> {
    if let Some(root) = std::env::var_os("NERV_PROJECT_ROOT") {
        load_api_keys_env(Path::new(&root));
    }
    let vendor = vendor(model_name);
    let key = api_key(vendor)?;
    match vendor {
        "anthropic" => {
            complete_anthropic(
                messages,
                model_name,
                tools,
                temperature,
                json_mode,
                &key,
                timeout,
            )
            .await
        }
        "gemini" => {
            complete_gemini(
                messages,
                model_name,
                tools,
                temperature,
                json_mode,
                &key,
                timeout,
            )
            .await
        }
        _ => {
            complete_openai_compatible(
                messages,
                model_name,
                vendor,
                tools,
                temperature,
                json_mode,
                &key,
                timeout,
            )
            .await
        }
    }
}

fn client(timeout: Duration) -> Result<reqwest::Client, LlmError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| LlmError::Unsupported(format!("http client: {e}")))
}

async fn post_json(url: &str, headers: &[(&str, String)], body: &Value) -> Result<Value, LlmError> {
    let mut request = client(Duration::from_secs(120))?.post(url).json(body);
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|e| LlmError::Unsupported(format!("request failed: {e}")))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|e| LlmError::Unsupported(format!("invalid response body: {e}")))?;
    if !status.is_success() {
        let message = body
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| body.to_string());
        return Err(LlmError::Unsupported(format!(
            "API error ({status}): {message}"
        )));
    }
    Ok(body)
}

fn parse_tool_calls_openai(message: &Value) -> Vec<ToolCall> {
    message
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .map(|calls| {
            calls
                .iter()
                .filter_map(|tc| {
                    let function = tc.get("function")?;
                    let name = function.get("name")?.as_str()?.to_string();
                    let arguments = match function.get("arguments") {
                        Some(Value::String(raw)) => serde_json::from_str(raw).unwrap_or(json!({})),
                        Some(other) => other.clone(),
                        None => json!({}),
                    };
                    Some(ToolCall {
                        function: FunctionCall { name, arguments },
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn json_instruction(json_mode: bool, vendor: &str) -> Option<String> {
    if !json_mode {
        return None;
    }
    // Only the OpenAI-compatible family takes a response_format; the others
    // get an explicit instruction (same contract, portable across vendors).
    let _ = vendor;
    Some(
        "Respond with ONLY a single valid JSON object. No prose, no markdown \
         fences — just the JSON."
            .to_string(),
    )
}

// ── OpenAI-compatible (openai, deepseek, …) ─────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn complete_openai_compatible(
    messages: &[ChatMessage],
    model_name: &str,
    vendor: &str,
    tools: Option<&[Value]>,
    temperature: f64,
    json_mode: bool,
    key: &str,
    _timeout: Duration,
) -> Result<CompletionResult, LlmError> {
    let url = match vendor {
        "deepseek" => "https://api.deepseek.com/chat/completions",
        _ => "https://api.openai.com/v1/chat/completions",
    };
    let mut body = json!({
        "model": model_name,
        "messages": messages,
        "temperature": temperature,
    });
    if json_mode {
        body["response_format"] = json!({"type": "json_object"});
    }
    if let Some(tools) = tools {
        body["tools"] = json!(tools);
    }
    let data = post_json(url, &[("Authorization", format!("Bearer {key}"))], &body).await?;
    let choice = data
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| LlmError::Unsupported("response has no choices".into()))?;
    let message = choice.get("message").cloned().unwrap_or(json!({}));
    Ok(CompletionResult {
        content: message
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        tool_calls: parse_tool_calls_openai(&message),
        runs_own_tool_loop: false,
        provider: format!("litellm:{vendor}"),
        duration_ms: None,
        cost_usd: None,
        usage: data.get("usage").cloned(),
    })
}

// ── Anthropic Messages API ──────────────────────────────────────────────────

fn anthropic_content_blocks(messages: &[ChatMessage]) -> (String, Vec<Value>) {
    let mut system = String::new();
    let mut out: Vec<Value> = Vec::new();
    for message in messages {
        match message.role.as_str() {
            "system" => {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(&message.content);
            }
            "assistant" => {
                let mut blocks = vec![json!({
                    "type": "text",
                    "text": message.content,
                })];
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": format!("call_{}", calls.iter().position(|c| std::ptr::eq(c, call)).unwrap_or_default()),
                            "name": call.function.name,
                            "input": call.function.arguments,
                        }));
                    }
                }
                out.push(json!({"role": "assistant", "content": blocks}));
            }
            "tool" => {
                // The tool result must reference the tool_use id of the
                // assistant turn before it; ids were synthesized on write.
                out.push(json!({
                    "role": "user",
                    "content": [{"type": "text", "text": message.content}],
                }));
            }
            _ => {
                out.push(json!({"role": "user", "content": message.content}));
            }
        }
    }
    (system, out)
}

fn anthropic_tools(tools: Option<&[Value]>) -> Vec<Value> {
    tools
        .unwrap_or(&[])
        .iter()
        .filter_map(|schema| {
            let function = schema.get("function")?;
            Some(json!({
                "name": function.get("name")?,
                "description": function.get("description").cloned().unwrap_or(json!("")),
                "input_schema": function.get("parameters").cloned().unwrap_or(json!({"type": "object"})),
            }))
        })
        .collect()
}

async fn complete_anthropic(
    messages: &[ChatMessage],
    model_name: &str,
    tools: Option<&[Value]>,
    temperature: f64,
    json_mode: bool,
    key: &str,
    _timeout: Duration,
) -> Result<CompletionResult, LlmError> {
    let (system, conversation) = anthropic_content_blocks(messages);
    let mut body = json!({
        "model": model_name,
        "max_tokens": 4096,
        "temperature": temperature,
        "messages": conversation,
    });
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    let tool_specs = anthropic_tools(tools);
    if !tool_specs.is_empty() {
        body["tools"] = json!(tool_specs);
    }
    if let Some(instruction) = json_instruction(json_mode, "anthropic") {
        body["system"] = json!(format!("{}\n\n{}", system, instruction));
    }

    let data = post_json(
        "https://api.anthropic.com/v1/messages",
        &[
            ("x-api-key", key.to_string()),
            ("anthropic-version", "2023-06-01".to_string()),
        ],
        &body,
    )
    .await?;

    let mut content = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    if let Some(blocks) = data.get("content").and_then(|v| v.as_array()) {
        for block in blocks {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    content.push_str(block.get("text").and_then(|v| v.as_str()).unwrap_or(""));
                }
                Some("tool_use") => {
                    tool_calls.push(ToolCall {
                        function: FunctionCall {
                            name: block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            arguments: block.get("input").cloned().unwrap_or(json!({})),
                        },
                    });
                }
                _ => {}
            }
        }
    }
    Ok(CompletionResult {
        content,
        tool_calls,
        runs_own_tool_loop: false,
        provider: "litellm:anthropic".to_string(),
        duration_ms: None,
        cost_usd: data.get("usage").and_then(|u| {
            let input = u.get("input_tokens")?.as_f64()?;
            let output = u.get("output_tokens")?.as_f64()?;
            Some(input * 3.0 / 1_000_000.0 + output * 15.0 / 1_000_000.0)
        }),
        usage: data.get("usage").cloned(),
    })
}

// ── Gemini GenerateContent ──────────────────────────────────────────────────

fn gemini_contents(messages: &[ChatMessage]) -> Vec<Value> {
    messages
        .iter()
        .filter(|m| m.role != "system" && m.role != "tool")
        .map(|m| {
            json!({
                "role": if m.role == "assistant" { "model" } else { "user" },
                "parts": [{"text": m.content}],
            })
        })
        .collect()
}

fn gemini_system(messages: &[ChatMessage]) -> Option<Value> {
    let system: Vec<&ChatMessage> = messages.iter().filter(|m| m.role == "system").collect();
    if system.is_empty() {
        return None;
    }
    Some(json!({"parts": system.iter().map(|m| json!({"text": m.content})).collect::<Vec<_>>()}))
}

fn gemini_tools(tools: Option<&[Value]>) -> Option<Value> {
    let declarations: Vec<Value> = tools
        .unwrap_or(&[])
        .iter()
        .filter_map(|schema| {
            let function = schema.get("function")?;
            Some(json!({
                "name": function.get("name")?,
                "description": function.get("description").cloned().unwrap_or(json!("")),
                "parameters": function.get("parameters").cloned().unwrap_or(json!({"type": "object"})),
            }))
        })
        .collect();
    (!declarations.is_empty()).then(|| json!({"functionDeclarations": declarations}))
}

async fn complete_gemini(
    messages: &[ChatMessage],
    model_name: &str,
    tools: Option<&[Value]>,
    temperature: f64,
    json_mode: bool,
    key: &str,
    _timeout: Duration,
) -> Result<CompletionResult, LlmError> {
    let model_name = model_name.trim_start_matches("models/");
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{model_name}:generateContent"
    );
    let mut body = json!({
        "contents": gemini_contents(messages),
        "generationConfig": {"temperature": temperature},
    });
    if let Some(system) = gemini_system(messages) {
        body["systemInstruction"] = system;
    }
    if json_mode {
        body["generationConfig"]["responseMimeType"] = json!("application/json");
    }
    if let Some(tools) = gemini_tools(tools) {
        body["tools"] = json!([tools]);
    }

    let data = post_json(&url, &[("x-goog-api-key", key.to_string())], &body).await?;
    let candidate = data
        .get("candidates")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(json!({}));
    let parts = candidate
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();

    let mut content = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for part in &parts {
        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
            content.push_str(text);
        }
        if let Some(function) = part.get("functionCall") {
            tool_calls.push(ToolCall {
                function: FunctionCall {
                    name: function
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    arguments: function.get("args").cloned().unwrap_or(json!({})),
                },
            });
        }
    }
    debug!("gemini turn done ({} parts)", parts.len());
    Ok(CompletionResult {
        content,
        tool_calls,
        runs_own_tool_loop: false,
        provider: "litellm:gemini".to_string(),
        duration_ms: None,
        cost_usd: None,
        usage: data.get("usageMetadata").cloned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_resolution_matches_parse_model() {
        assert_eq!(vendor("claude-opus-4-8"), "anthropic");
        assert_eq!(vendor("anthropic/claude-opus-4-8"), "anthropic");
        assert_eq!(vendor("gpt-5.2"), "openai");
        assert_eq!(vendor("openai/o3-mini"), "openai");
        assert_eq!(vendor("deepseek/deepseek-chat"), "deepseek");
        assert_eq!(vendor("gemini/gemini-2.5-pro"), "gemini");
        assert_eq!(vendor("gemini-2.5-pro"), "gemini");
        // Unknown vendors default to the OpenAI-compatible family.
        assert_eq!(vendor("mistral/mistral-large"), "openai");
    }

    #[test]
    fn anthropic_content_extracts_system_and_conversation() {
        let messages = vec![
            ChatMessage::new("system", "You are Nerv."),
            ChatMessage::new("user", "hello"),
            ChatMessage::new("assistant", "hi there"),
            ChatMessage::new("user", "bye"),
        ];
        let (system, conversation) = anthropic_content_blocks(&messages);
        assert_eq!(system, "You are Nerv.");
        assert_eq!(conversation.len(), 3);
        assert_eq!(conversation[0]["role"], "user");
        assert_eq!(conversation[1]["role"], "assistant");
    }

    #[test]
    fn anthropic_tools_map_from_openai_schemas() {
        let schemas = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web",
                "parameters": {"type": "object", "properties": {}},
            }
        })];
        let mapped = anthropic_tools(Some(&schemas));
        assert_eq!(mapped[0]["name"], "web_search");
        assert_eq!(mapped[0]["input_schema"]["type"], "object");
    }

    #[test]
    fn openai_tool_calls_parse_string_arguments() {
        let message = serde_json::json!({
            "tool_calls": [
                {"function": {"name": "shell", "arguments": "{\"command\": \"ls\"}"}},
                {"function": {"name": "browser", "arguments": {"url": "https://x.org"}}},
            ]
        });
        let calls = parse_tool_calls_openai(&message);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.arguments["command"], "ls");
        assert_eq!(calls[1].function.arguments["url"], "https://x.org");
    }

    #[test]
    fn missing_api_key_is_a_clear_error() {
        let saved = std::env::var("ANTHROPIC_API_KEY").ok();
        std::env::remove_var("ANTHROPIC_API_KEY");
        let err = api_key("anthropic").unwrap_err().to_string();
        assert!(err.contains("ANTHROPIC_API_KEY"), "{err}");
        if let Some(saved) = saved {
            std::env::set_var("ANTHROPIC_API_KEY", saved);
        }
    }
}
