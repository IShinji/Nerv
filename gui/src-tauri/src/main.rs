// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nerv_brain::handle::BrainHandle;
use nerv_shared::config::NervConfig;
use serde::Serialize;
use tauri::State;
use tokio::sync::Mutex;

struct AppState {
    /// The fully-native brain facade, built on first use. There is no
    /// sidecar to supervise — everything runs in this process.
    handle: Mutex<Option<Arc<BrainHandle>>>,
    project_root: PathBuf,
}

impl AppState {
    /// Return the brain facade, building it once on first use.
    async fn handle(&self) -> Result<Arc<BrainHandle>, String> {
        let mut guard = self.handle.lock().await;
        if let Some(handle) = guard.as_ref() {
            return Ok(handle.clone());
        }
        let handle = Arc::new(BrainHandle::new(&self.project_root).map_err(|e| e.to_string())?);
        *guard = Some(handle.clone());
        Ok(handle)
    }
}

#[derive(Serialize)]
struct Agent {
    name: String,
    description: String,
}

/// Send a message; the full result (reply + model/provider/tokens/cost
/// metadata) goes back for the per-message annotation.
#[tauri::command]
async fn send_message(
    state: State<'_, AppState>,
    text: String,
) -> Result<serde_json::Value, String> {
    state
        .handle()
        .await?
        .reply(&text, "gui", "gui-user")
        .await
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct FilePayload {
    pub name: String,
    /// UTF-8 content when the file is a readable text file; None for binary.
    pub text: Option<String>,
    pub size_bytes: u64,
    pub truncated: bool,
}

/// The most text we inline into a prompt from a dropped file.
const DROP_FILE_MAX_BYTES: u64 = 256 * 1024;

/// Read a dropped file for the chat composer: text files are inlined
/// (capped), binary files come back as `text: None` and Nerv reads them
/// through its own file tools by path instead.
#[tauri::command]
async fn read_dropped_file(path: String) -> Result<FilePayload, String> {
    read_dropped_payload(std::path::Path::new(&path))
}

/// The sync core of [`read_dropped_file`] (testable without the runtime).
fn read_dropped_payload(candidate: &std::path::Path) -> Result<FilePayload, String> {
    let path = candidate.display().to_string();
    let meta = std::fs::metadata(candidate).map_err(|e| format!("{}: {e}", path))?;
    let size = meta.len();
    let name = candidate
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());

    if size > DROP_FILE_MAX_BYTES {
        return Ok(FilePayload {
            name,
            text: None,
            size_bytes: size,
            truncated: false,
        });
    }
    let bytes = std::fs::read(&candidate).map_err(|e| format!("{}: {e}", path))?;
    // Binary heuristic: a NUL byte in the head disqualifies inlining.
    let head = &bytes[..bytes.len().min(8192)];
    let binary = bytes.is_empty() || head.contains(&0u8);
    if binary {
        return Ok(FilePayload {
            name,
            text: None,
            size_bytes: size,
            truncated: false,
        });
    }
    let mut text = String::from_utf8_lossy(&bytes).to_string();
    let mut truncated = false;
    if text.chars().count() > 100_000 {
        text = text.chars().take(100_000).collect::<String>() + "\n… [truncated]";
        truncated = true;
    }
    Ok(FilePayload {
        name,
        text: Some(text),
        size_bytes: size,
        truncated,
    })
}

/// Install an agent YAML from a URL (Agent Store client capability): fetch,
/// validate by parsing, save into personal/agents.
#[tauri::command]
async fn install_agent(
    state: State<'_, AppState>,
    url: String,
    name: String,
) -> Result<String, String> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err("url must be an http(s) URL".into());
    }
    let client = reqwest::Client::new();
    let body = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("fetch failed: {e}"))?
        .text()
        .await
        .map_err(|e| format!("read failed: {e}"))?;
    let agent: nerv_brain::orchestrator::AgentDefinition =
        serde_yaml::from_str(&body).map_err(|e| format!("Invalid agent YAML: {e}"))?;
    if agent.name.trim().is_empty() {
        return Err("the agent has no name".into());
    }
    let slug = name
        .trim()
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let dir = state.project_root.join("personal").join("agents");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!(
        "{}.yaml",
        if slug.is_empty() {
            agent.name.to_lowercase().replace(' ', "-")
        } else {
            slug
        }
    ));
    std::fs::write(&path, &body).map_err(|e| e.to_string())?;
    Ok(format!(
        "Agent '{}' installed into {}",
        agent.name,
        path.display()
    ))
}

/// The full YAML of one agent (for the agent editor).
#[tauri::command]
async fn get_agent(state: State<'_, AppState>, name: String) -> Result<String, String> {
    // Personal first, then preset — same precedence as the registry.
    let personal = state.project_root.join("personal").join("agents");
    let preset = state.project_root.join("agents");
    for dir in [personal, preset] {
        let path = dir.join(format!(
            "{}.yaml",
            name.trim().to_lowercase().replace(' ', "-")
        ));
        if path.exists() {
            return std::fs::read_to_string(&path).map_err(|e| e.to_string());
        }
    }
    Err(format!("No agent named '{name}'."))
}

/// Save an edited agent YAML into personal/agents (validated by parsing).
#[tauri::command]
async fn save_agent(state: State<'_, AppState>, name: String, yaml: String) -> Result<(), String> {
    // Parse first: an invalid YAML must never reach the registry.
    let _: nerv_brain::orchestrator::AgentDefinition =
        serde_yaml::from_str(&yaml).map_err(|e| format!("Invalid agent YAML: {e}"))?;
    let dir = state.project_root.join("personal").join("agents");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let slug = name.trim().to_lowercase().replace(' ', "-");
    std::fs::write(dir.join(format!("{slug}.yaml")), yaml).map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct RequestRecord {
    pub tier: Option<i64>,
    pub model: String,
    pub provider: String,
    pub est_input_tokens: i64,
    pub cost_usd: Option<f64>,
    pub duration_ms: f64,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct SystemStats {
    pub requests: u64,
    pub est_input_tokens: i64,
    pub cost_usd: f64,
    pub recent: Vec<RequestRecord>,
}

/// Dashboard breakdown (PRD 4.4): aggregate the whole trace log per day
/// and per model.
#[tauri::command]
async fn stats_breakdown(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let path = state
        .project_root
        .join("personal")
        .join("logs")
        .join("trace.jsonl");
    let body = std::fs::read_to_string(&path).unwrap_or_default();
    #[derive(Default, serde::Serialize)]
    struct Bucket {
        requests: u64,
        est_input_tokens: i64,
        cost_usd: f64,
        errors: u64,
    }
    let mut per_day: std::collections::BTreeMap<String, Bucket> = Default::default();
    let mut per_model: std::collections::BTreeMap<String, Bucket> = Default::default();
    let mut per_agent: std::collections::BTreeMap<String, Bucket> = Default::default();
    for line in body.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let day = value
            .get("ts")
            .and_then(|v| v.as_str())
            .map(|ts| ts.chars().take(10).collect::<String>())
            .unwrap_or_else(|| "unknown".to_string());
        let model = value
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let tokens = value
            .get("est_input_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let cost = value
            .get("cost_usd")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let is_error = value.get("error").is_some_and(|e| !e.is_null());
        let agent = value
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        for bucket in [
            per_day.entry(day).or_default(),
            per_model.entry(model).or_default(),
            per_agent.entry(agent).or_default(),
        ] {
            bucket.requests += 1;
            bucket.est_input_tokens += tokens;
            bucket.cost_usd += cost;
            if is_error {
                bucket.errors += 1;
            }
        }
    }
    Ok(serde_json::json!({
        "per_day": per_day,
        "per_model": per_model,
        "per_agent": per_agent,
    }))
}

/// The raw user config.yaml for the settings-page editor (empty when the
/// file doesn't exist yet — the defaults apply).
#[tauri::command]
async fn get_config_yaml(state: State<'_, AppState>) -> Result<String, String> {
    let path = state.project_root.join("config.yaml");
    if path.exists() {
        std::fs::read_to_string(&path).map_err(|e| e.to_string())
    } else {
        Ok(String::new())
    }
}

/// Save an edited config.yaml: parse-validated against NervConfig (merged
/// with the defaults) before the write lands; file mode tightened to 0600.
#[tauri::command]
async fn save_config_yaml(state: State<'_, AppState>, yaml: String) -> Result<(), String> {
    let defaults =
        std::fs::read_to_string(state.project_root.join("core").join("config.default.yaml"))
            .map_err(|e| format!("cannot read the default config: {e}"))?;
    let mut merged_value: serde_yaml::Value =
        serde_yaml::from_str(&defaults).map_err(|e| format!("default config invalid: {e}"))?;
    let user_value: serde_yaml::Value =
        serde_yaml::from_str(&yaml).map_err(|e| format!("YAML syntax error: {e}"))?;
    if let (serde_yaml::Value::Mapping(base), serde_yaml::Value::Mapping(over)) =
        (&mut merged_value, &user_value)
    {
        for (k, v) in over {
            base.insert(k.clone(), v.clone());
        }
    }
    nerv_shared::config::NervConfig::load(&state.project_root)
        .map_err(|e| format!("config invalid: {e}"))?;
    let path = state.project_root.join("config.yaml");
    std::fs::write(&path, &yaml).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Data export (PRD 4.4): bundle every personal/ text file into one
/// timestamped JSON archive under personal/exports/.
#[tauri::command]
async fn export_data(state: State<'_, AppState>) -> Result<String, String> {
    let personal = state.project_root.join("personal");
    let mut files: Vec<(String, String)> = Vec::new();
    fn collect(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<(String, String)>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect(&path, base, out);
                } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    matches!(e, "yaml" | "md" | "json" | "jsonl" | "ics" | "env" | "txt")
                }) && !path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().ends_with(".lock"))
                {
                    if let Ok(body) = std::fs::read_to_string(&path) {
                        let rel = path
                            .strip_prefix(base)
                            .unwrap_or(&path)
                            .display()
                            .to_string();
                        out.push((rel, body));
                    }
                }
            }
        }
    }
    collect(&personal, &personal, &mut files);
    files.sort();
    let archive = serde_json::json!({
        "schema_version": 1,
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "files": files.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
        "contents": files,
    });
    let out_dir = personal.join("exports");
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let name = format!(
        "nerv-export-{}.json",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    );
    let path = out_dir.join(&name);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&archive).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(format!(
        "Exported {} file(s) to {}",
        files.len(),
        path.display()
    ))
}

/// Data import (PRD 4.4): restore a `nerv-export-*.json` archive produced by
/// [`export_data`]. Every archive file overwrites the matching personal/
/// path — the caller is expected to point this at their own archive.
#[tauri::command]
async fn import_data(state: State<'_, AppState>, archive: String) -> Result<String, String> {
    let body = std::fs::read_to_string(&archive).map_err(|e| format!("{}: {e}", archive))?;
    let data: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("not a Nerv export archive: {e}"))?;
    if data.get("schema_version").and_then(|v| v.as_i64()) != Some(1) {
        return Err("unsupported archive schema_version".into());
    }
    let contents = data
        .get("contents")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "archive has no contents".to_string())?;
    let personal = state.project_root.join("personal");
    let mut restored = 0usize;
    for entry in contents {
        let Some(pair) = entry.as_array() else {
            continue;
        };
        let (Some(rel), Some(body)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let (Some(rel), Some(body)) = (rel.as_str(), body.as_str()) else {
            continue;
        };
        // Fail closed on traversal: the restored path must stay in personal/.
        let target = personal.join(rel);
        if !target.starts_with(&personal) {
            return Err(format!("archive path escapes personal/: {rel}"));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&target, body).map_err(|e| e.to_string())?;
        if rel.ends_with("api_keys.env") || rel.ends_with("config.yaml") {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| e.to_string())?;
            }
        }
        restored += 1;
    }
    Ok(format!("Imported {restored} file(s) into personal/"))
}

/// Token/cost dashboard from the trace log (the newest entries win).
#[tauri::command]
async fn system_stats(state: State<'_, AppState>) -> Result<SystemStats, String> {
    let path = state
        .project_root
        .join("personal")
        .join("logs")
        .join("trace.jsonl");
    let Ok(body) = std::fs::read_to_string(&path) else {
        return Ok(SystemStats {
            requests: 0,
            est_input_tokens: 0,
            cost_usd: 0.0,
            recent: Vec::new(),
        });
    };
    let mut records: Vec<RequestRecord> = Vec::new();
    for line in body.lines().rev() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        records.push(RequestRecord {
            tier: value.get("tier").and_then(|v| v.as_i64()),
            model: value
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            provider: value
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            est_input_tokens: value
                .get("est_input_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            cost_usd: value.get("cost_usd").and_then(|v| v.as_f64()),
            duration_ms: value
                .get("duration_ms")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            error: value
                .get("error")
                .filter(|e| !e.is_null())
                .and_then(|e| e.as_str())
                .map(str::to_string),
        });
    }
    let requests = records.len() as u64;
    let est_input_tokens = records.iter().map(|r| r.est_input_tokens).sum();
    let cost_usd = records.iter().filter_map(|r| r.cost_usd).sum();
    records.truncate(20);
    Ok(SystemStats {
        requests,
        est_input_tokens,
        cost_usd,
        recent: records,
    })
}

/// Hardware profile + model recommendation for the setup wizard.
#[tauri::command]
async fn hardware_profile() -> Result<nerv_brain::llm::ollama::HardwareProfile, String> {
    Ok(nerv_brain::llm::ollama::hardware_profile())
}

/// The models installed in the local Ollama instance.
#[tauri::command]
async fn ollama_models(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let config = NervConfig::load(&state.project_root).map_err(|e| e.to_string())?;
    Ok(nerv_brain::llm::ollama::ollama_installed_models(&config.models.router.base_url).await)
}

/// One-click model download through the local Ollama instance.
#[tauri::command]
async fn ollama_pull(state: State<'_, AppState>, model: String) -> Result<(), String> {
    let config = NervConfig::load(&state.project_root).map_err(|e| e.to_string())?;
    nerv_brain::llm::ollama::ollama_pull(&config.models.router.base_url, &model).await
}

/// Delete a local Ollama model (模型管理: 删).
#[tauri::command]
async fn ollama_delete(state: State<'_, AppState>, model: String) -> Result<(), String> {
    let config = NervConfig::load(&state.project_root).map_err(|e| e.to_string())?;
    let base = config.models.router.base_url;
    let response = reqwest::Client::new()
        .delete(format!("{base}/api/delete"))
        .json(&serde_json::json!({"name": model}))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    if body.get("error").is_some() {
        return Err(format!(
            "ollama delete failed: {}",
            body["error"].as_str().unwrap_or("?")
        ));
    }
    Ok(())
}

/// Wizard step 3: persist cloud API keys into personal/api_keys.env
/// (merged; existing values not in this submission are kept).
#[tauri::command]
async fn save_api_keys(
    state: State<'_, AppState>,
    keys: std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let path = state.project_root.join("personal").join("api_keys.env");
    merge_api_keys(&path, keys)
}

/// Merge new keys into an api_keys.env file: existing values not in this
/// submission are kept, empty submissions are ignored, result is sorted.
fn merge_api_keys(
    path: &std::path::Path,
    keys: std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let mut existing: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Ok(body) = std::fs::read_to_string(path) {
        for line in body.lines() {
            if let Some((k, v)) = line.split_once('=') {
                existing.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    for (k, v) in keys {
        let v = v.trim().to_string();
        if v.is_empty() {
            continue;
        }
        existing.insert(k, v);
    }
    std::fs::create_dir_all(path.parent().expect("has parent")).map_err(|e| e.to_string())?;
    let mut lines: Vec<String> = existing.iter().map(|(k, v)| format!("{k}={v}")).collect();
    lines.sort();
    std::fs::write(path, lines.join("\n") + "\n").map_err(|e| e.to_string())
}

/// Wizard step 4: write the channels block into the user config (merged
/// into the existing YAML; file mode tightened to 0600 — it holds tokens).
#[tauri::command]
async fn save_channels(
    state: State<'_, AppState>,
    channels: serde_json::Value,
) -> Result<(), String> {
    let path = state.project_root.join("config.yaml");
    let mut merged: serde_yaml::Value = if path.exists() {
        serde_yaml::from_str(&std::fs::read_to_string(&path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("existing config.yaml is not valid YAML: {e}"))?
    } else {
        serde_yaml::Value::Mapping(Default::default())
    };
    let channels_json = serde_json::to_string(&channels).map_err(|e| e.to_string())?;
    let channels_value: serde_yaml::Value =
        serde_yaml::from_str(&channels_json).map_err(|e| e.to_string())?;
    if let serde_yaml::Value::Mapping(root) = &mut merged {
        if let serde_yaml::Value::Mapping(block) = channels_value {
            for (k, v) in block {
                root.insert(k, v);
            }
        }
    } else {
        return Err("config.yaml root must be a mapping".into());
    }
    let body = serde_yaml::to_string(&merged).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Dashboard: real-time memory/CPU + RAM total.
#[tauri::command]
async fn memory_stats() -> Result<serde_json::Value, String> {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_cpu_usage();
    Ok(serde_json::json!({
        "total_ram_gb": sys.total_memory() as f64 / 1024.0 / 1024.0 / 1024.0,
        "used_ram_gb": sys.used_memory() as f64 / 1024.0 / 1024.0 / 1024.0,
        "cpu_percent": sys.global_cpu_usage(),
    }))
}

/// Dashboard: models loaded in the local Ollama instance (/api/ps).
#[tauri::command]
async fn ollama_loaded(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let config = NervConfig::load(&state.project_root).map_err(|e| e.to_string())?;
    let base = config.models.router.base_url;
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/ps"))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(body
        .get("models")
        .and_then(|v| v.as_array())
        .map(|models| {
            models
                .iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

/// Dashboard: the background-task queue (delegations and the like).
#[derive(Serialize)]
pub struct TaskRow {
    pub id: String,
    pub kind: String,
    pub status: String,
}

#[tauri::command]
async fn task_queue(state: State<'_, AppState>) -> Result<Vec<TaskRow>, String> {
    let store = nerv_shared::background_tasks::BackgroundTaskStore::new(&state.project_root);
    let mut rows: Vec<TaskRow> = store
        .list_by_status(nerv_shared::background_tasks::QUEUED)
        .into_iter()
        .map(|t| TaskRow {
            id: t.id,
            kind: t.kind,
            status: t.status,
        })
        .collect();
    rows.extend(
        store
            .list_by_status(nerv_shared::background_tasks::RUNNING)
            .into_iter()
            .map(|t| TaskRow {
                id: t.id,
                kind: t.kind,
                status: t.status,
            }),
    );
    Ok(rows)
}

#[tauri::command]
async fn check_heartbeat() -> Result<String, String> {
    // The heartbeat drain is native now: surface queued background alerts.
    Ok(nerv_brain::proactive::drain_notifications().join("\n"))
}

#[tauri::command]
async fn list_agents(state: State<'_, AppState>) -> Result<Vec<Agent>, String> {
    let mut agents = Vec::new();
    for sub in ["agents", "personal/agents"] {
        let dir = state.project_root.join(sub);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                let name = extract_field(&text, "name").unwrap_or_else(|| {
                    path.file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                });
                let description = extract_field(&text, "description").unwrap_or_default();
                agents.push(Agent { name, description });
            }
        }
    }
    Ok(agents)
}

/// Extract a top-level `key: value` string from minimal YAML (no YAML dep).
fn extract_field(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&prefix) {
            let value = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// Find the Nerv project root: `NERV_PROJECT_ROOT` wins (a packaged app
/// launched from Finder has an unrelated cwd), otherwise walk up from the
/// working directory.
fn find_project_root() -> PathBuf {
    resolve_project_root(
        std::env::var_os("NERV_PROJECT_ROOT"),
        std::env::current_dir().ok(),
    )
}

fn resolve_project_root(env_root: Option<OsString>, cwd: Option<PathBuf>) -> PathBuf {
    if let Some(root) = env_root.filter(|r| !r.is_empty()) {
        return PathBuf::from(root);
    }
    if let Some(cwd) = cwd {
        for ancestor in cwd.ancestors() {
            if ancestor.join("brain").is_dir() && ancestor.join("core").is_dir() {
                return ancestor.to_path_buf();
            }
        }
    }
    PathBuf::from("..")
}

/// PATH for a child process. An app started from Finder inherits a minimal
/// PATH, so Homebrew/uv/rustup binaries would not be found; prepend the
/// usual user bin dirs (without duplicating entries). Retained with its
/// tests after the python sidecar retired.
#[allow(dead_code)]
fn child_path(existing: Option<&str>, home: Option<&Path>) -> String {
    let mut dirs: Vec<String> = vec!["/opt/homebrew/bin".into(), "/usr/local/bin".into()];
    if let Some(home) = home {
        dirs.push(home.join(".local/bin").to_string_lossy().into_owned());
        dirs.push(home.join(".cargo/bin").to_string_lossy().into_owned());
    }
    let mut out: Vec<String> = Vec::new();
    for dir in dirs
        .into_iter()
        .chain(existing.unwrap_or("").split(':').map(str::to_string))
    {
        if !dir.is_empty() && !out.contains(&dir) {
            out.push(dir);
        }
    }
    out.join(":")
}

fn main() {
    // The GUI binary doubles as the rust MCP server: when the claude-cli
    // bridge spawns this executable in --mcp-server mode, run the server
    // loop instead of the desktop app.
    if std::env::args().any(|arg| arg == "--mcp-server") {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("mcp server runtime");
        runtime.block_on(nerv_brain::mcp_server::run_stdio());
        return;
    }

    let project_root = find_project_root();

    tauri::Builder::default()
        .manage(AppState {
            handle: Mutex::new(None),
            project_root,
        })
        .invoke_handler(tauri::generate_handler![
            send_message,
            check_heartbeat,
            list_agents,
            get_agent,
            save_agent,
            install_agent,
            read_dropped_file,
            system_stats,
            hardware_profile,
            ollama_models,
            ollama_pull,
            ollama_delete,
            save_api_keys,
            save_channels,
            memory_stats,
            ollama_loaded,
            task_queue,
            stats_breakdown,
            get_config_yaml,
            save_config_yaml,
            export_data,
            import_data
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Nerv GUI");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_root_prefers_env_override() {
        let root = resolve_project_root(Some("/srv/nerv".into()), Some(PathBuf::from("/")));
        assert_eq!(root, PathBuf::from("/srv/nerv"));
    }

    #[test]
    fn project_root_ignores_empty_env_and_walks_up_from_cwd() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("brain")).unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        let nested = dir.path().join("gui").join("src-tauri");
        std::fs::create_dir_all(&nested).unwrap();
        let root = resolve_project_root(Some("".into()), Some(nested));
        assert_eq!(root, dir.path());
    }

    #[test]
    fn child_path_prepends_common_bins_without_duplicates() {
        let path = child_path(
            Some("/usr/bin:/opt/homebrew/bin:/bin"),
            Some(Path::new("/Users/me")),
        );
        assert_eq!(
            path,
            "/opt/homebrew/bin:/usr/local/bin:/Users/me/.local/bin:/Users/me/.cargo/bin:/usr/bin:/bin"
        );
    }

    #[test]
    fn child_path_works_with_minimal_environment() {
        assert_eq!(child_path(None, None), "/opt/homebrew/bin:/usr/local/bin");
    }

    #[test]
    fn extract_field_reads_a_quoted_value() {
        let yaml = "name: \"Coder\"\ndescription: 'Writes code'\n";
        assert_eq!(extract_field(yaml, "name").as_deref(), Some("Coder"));
        assert_eq!(
            extract_field(yaml, "description").as_deref(),
            Some("Writes code")
        );
    }

    #[test]
    fn extract_field_ignores_missing_and_empty_values() {
        assert_eq!(extract_field("name:\n", "name"), None);
        assert_eq!(extract_field("other: 1\n", "name"), None);
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn dropped_text_file_is_inlined() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "hello from a text file").unwrap();

        let payload = read_dropped_payload(&path).unwrap();
        assert_eq!(payload.text.as_deref(), Some("hello from a text file"));
        assert!(!payload.truncated);
    }

    #[test]
    fn dropped_binary_file_returns_no_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");
        let mut bytes = vec![b'x'; 200];
        bytes[100] = 0; // NUL in the head disqualifies inlining
        std::fs::write(&path, bytes).unwrap();

        let payload = read_dropped_payload(&path).unwrap();
        assert!(payload.text.is_none());
    }

    #[test]
    fn dropped_oversize_file_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        std::fs::write(&path, vec![b'a'; (DROP_FILE_MAX_BYTES + 1) as usize]).unwrap();

        let payload = read_dropped_payload(&path).unwrap();
        assert!(payload.text.is_none(), "oversize file must not be inlined");
        assert_eq!(payload.size_bytes, DROP_FILE_MAX_BYTES + 1);
    }

    #[test]
    fn api_keys_merge_keeps_existing_and_ignores_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api_keys.env");
        std::fs::write(
            &path,
            "ANTHROPIC_API_KEY=old-value\nGEMINI_API_KEY=keepme\n",
        )
        .unwrap();

        let mut keys = std::collections::HashMap::new();
        keys.insert("ANTHROPIC_API_KEY".to_string(), "new-value".to_string());
        keys.insert("OPENAI_API_KEY".to_string(), "  ".to_string()); // empty → ignored
        merge_api_keys(&path, keys).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("ANTHROPIC_API_KEY=new-value"));
        assert!(body.contains("GEMINI_API_KEY=keepme"));
        assert!(!body.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn api_keys_merge_creates_the_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("personal").join("api_keys.env");

        let mut keys = std::collections::HashMap::new();
        keys.insert("DEEPSEEK_API_KEY".to_string(), "sk-test".to_string());
        merge_api_keys(&path, keys).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body, "DEEPSEEK_API_KEY=sk-test\n");
    }

    #[test]
    fn dropped_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.txt");
        assert!(read_dropped_payload(&path).is_err());
        // Silence unused-import warnings for the Path alias in test scope.
        let _ = std::path::Path::new(".");
    }
}
