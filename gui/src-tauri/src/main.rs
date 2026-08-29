// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use nerv_shared::brain_client::{BrainClient, BrainSpawnConfig};
use nerv_shared::config::NervConfig;
use serde::Serialize;
use tauri::State;
use tokio::sync::Mutex;

struct AppState {
    /// Spawned on first use and supervised from then on.
    brain: Mutex<Option<Arc<BrainClient>>>,
    project_root: PathBuf,
}

impl AppState {
    /// Return the running brain, starting it on first use.
    async fn brain(&self) -> Result<Arc<BrainClient>, String> {
        let mut guard = self.brain.lock().await;
        if let Some(brain) = guard.as_ref() {
            return Ok(brain.clone());
        }

        let brain = Arc::new(BrainClient::new(brain_spawn_config(&self.project_root)));
        brain.start().await.map_err(|e| e.to_string())?;
        *guard = Some(brain.clone());
        Ok(brain)
    }

    /// Ask the brain for a `reply` string.
    async fn reply(&self, method: &str, params: serde_json::Value) -> Result<String, String> {
        let brain = self.brain().await?;
        let result = brain
            .call(method, params)
            .await
            .map_err(|e| e.to_string())?;
        Ok(result
            .get("reply")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }
}

/// Build the brain spawn config from `config.yaml`, exactly like the gateway.
///
/// Reading the same config is the point: the GUI used to hardcode
/// `uv run python -m nerv`, so a user who changed the interpreter or module in
/// config got a different brain depending on which host launched it.
fn brain_spawn_config(project_root: &Path) -> BrainSpawnConfig {
    let brain_dir = project_root.join("brain");

    let config = match NervConfig::load(project_root) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("[nerv-gui] falling back to default brain settings: {e}");
            return BrainSpawnConfig::python("uv run python", "nerv", &brain_dir)
                .with_env("NERV_PROJECT_ROOT", project_root.to_string_lossy());
        }
    };

    BrainSpawnConfig::python(
        &config.brain.python_command,
        &config.brain.module,
        &brain_dir,
    )
    .with_restart_max(config.brain.restart_max)
    .with_call_timeout(Duration::from_secs(config.brain.call_timeout_secs))
    .with_env("NERV_PROJECT_ROOT", project_root.to_string_lossy())
    .with_env("NERV_ROUTER_MODEL", &config.models.router.model)
    .with_env("NERV_OLLAMA_URL", &config.models.router.base_url)
    .with_env("NERV_MODEL_TIER0", &config.models.tiers.tier0)
    .with_env("NERV_MODEL_TIER1", &config.models.tiers.tier1)
    .with_env("NERV_MODEL_TIER2", &config.models.tiers.tier2)
    .with_env("NERV_MODEL_TIER3", &config.models.tiers.tier3)
}

#[derive(Serialize)]
struct Agent {
    name: String,
    description: String,
}

#[tauri::command]
async fn send_message(state: State<'_, AppState>, text: String) -> Result<String, String> {
    state
        .reply(
            "route",
            serde_json::json!({"message": text, "channel": "gui", "sender": "gui-user"}),
        )
        .await
}

#[tauri::command]
async fn check_heartbeat(state: State<'_, AppState>) -> Result<String, String> {
    state.reply("heartbeat", serde_json::json!({})).await
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

/// Walk up from the working directory to find the Nerv project root.
fn find_project_root() -> PathBuf {
    if let Ok(cwd) = std::env::current_dir() {
        for ancestor in cwd.ancestors() {
            if ancestor.join("brain").is_dir() && ancestor.join("core").is_dir() {
                return ancestor.to_path_buf();
            }
        }
    }
    PathBuf::from("..")
}

fn main() {
    let project_root = find_project_root();

    tauri::Builder::default()
        .manage(AppState {
            brain: Mutex::new(None),
            project_root,
        })
        .invoke_handler(tauri::generate_handler![
            send_message,
            check_heartbeat,
            list_agents
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Nerv GUI");
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn brain_spawn_config_falls_back_when_config_is_absent() {
        let dir = tempfile::TempDir::new().unwrap();
        let spawn = brain_spawn_config(dir.path());

        assert_eq!(spawn.command, "uv run python");
        assert_eq!(spawn.args, vec!["-m", "nerv"]);
        assert_eq!(spawn.working_dir, dir.path().join("brain"));
        assert!(spawn
            .env
            .iter()
            .any(|(k, v)| k == "NERV_PROJECT_ROOT" && v == &dir.path().to_string_lossy()));
    }

    #[test]
    fn brain_spawn_config_follows_the_shared_config_file() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(
            dir.path().join("core").join("config.default.yaml"),
            r#"
channels:
  cli:
    enabled: true
models:
  router:
    model: "claude-cli:opus"
brain:
  python_command: "python3"
  module: "nerv"
  call_timeout_secs: 42
"#,
        )
        .unwrap();

        let spawn = brain_spawn_config(dir.path());

        // The GUI must honor the same interpreter the gateway would use.
        assert_eq!(spawn.command, "python3");
        assert_eq!(spawn.call_timeout, Duration::from_secs(42));
        assert!(spawn
            .env
            .iter()
            .any(|(k, v)| k == "NERV_MODEL_TIER0" && v == "claude-cli:opus"));
    }
}
