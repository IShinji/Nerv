// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

use serde::Serialize;
use tauri::State;

/// A long-lived Python brain subprocess spoken to over newline-delimited JSON-RPC.
struct Brain {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Brain {
    fn spawn(brain_dir: &PathBuf, project_root: &PathBuf) -> std::io::Result<Self> {
        let mut child = Command::new("uv")
            .args(["run", "python", "-m", "nerv"])
            .current_dir(brain_dir)
            .env("NERV_PROJECT_ROOT", project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take().expect("brain stdin");
        let stdout = BufReader::new(child.stdout.take().expect("brain stdout"));
        Ok(Brain {
            _child: child,
            stdin,
            stdout,
            next_id: 1,
        })
    }

    fn call(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        });
        let line = serde_json::to_string(&req).map_err(|e| e.to_string())?;
        self.stdin.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        self.stdin.write_all(b"\n").map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())?;

        loop {
            let mut buf = String::new();
            let n = self.stdout.read_line(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("brain process closed".into());
            }
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }
            let val: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue, // ignore non-JSON log noise on stdout
            };
            if val.get("id").and_then(|v| v.as_u64()) == Some(id) {
                if let Some(err) = val.get("error") {
                    return Err(err.to_string());
                }
                return Ok(val.get("result").cloned().unwrap_or(serde_json::Value::Null));
            }
        }
    }
}

struct AppState {
    brain: Mutex<Option<Brain>>,
    brain_dir: PathBuf,
    project_root: PathBuf,
}

impl AppState {
    /// Lazily spawn the brain on first use, then run `f` against it.
    fn with_brain<R>(
        &self,
        f: impl FnOnce(&mut Brain) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut guard = self.brain.lock().map_err(|e| e.to_string())?;
        if guard.is_none() {
            let brain = Brain::spawn(&self.brain_dir, &self.project_root).map_err(|e| e.to_string())?;
            *guard = Some(brain);
        }
        f(guard.as_mut().expect("brain present"))
    }
}

#[derive(Serialize)]
struct Agent {
    name: String,
    description: String,
}

#[tauri::command]
fn send_message(state: State<AppState>, text: String) -> Result<String, String> {
    state.with_brain(|brain| {
        let result = brain.call(
            "route",
            serde_json::json!({"message": text, "channel": "gui", "sender": "gui-user"}),
        )?;
        Ok(result
            .get("reply")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    })
}

#[tauri::command]
fn check_heartbeat(state: State<AppState>) -> Result<String, String> {
    state.with_brain(|brain| {
        let result = brain.call("heartbeat", serde_json::json!({}))?;
        Ok(result
            .get("reply")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    })
}

#[tauri::command]
fn list_agents(state: State<AppState>) -> Result<Vec<Agent>, String> {
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
                    path.file_stem().unwrap_or_default().to_string_lossy().to_string()
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
    let brain_dir = project_root.join("brain");

    tauri::Builder::default()
        .manage(AppState {
            brain: Mutex::new(None),
            brain_dir,
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
