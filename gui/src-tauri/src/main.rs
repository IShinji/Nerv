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

#[tauri::command]
async fn send_message(state: State<'_, AppState>, text: String) -> Result<String, String> {
    let result = state
        .handle()
        .await?
        .reply(&text, "gui", "gui-user")
        .await
        .map_err(|e| e.to_string())?;
    Ok(result
        .get("reply")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string())
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
            system_stats,
            hardware_profile,
            ollama_models,
            ollama_pull
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
