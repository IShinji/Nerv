use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config: {0}")]
    Parse(#[from] serde_yaml::Error),
}

/// Top-level Nerv configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NervConfig {
    pub channels: ChannelsConfig,
    pub models: ModelsConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub brain: BrainConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelsConfig {
    #[serde(default)]
    pub cli: CliChannelConfig,
    #[serde(default)]
    pub telegram: TelegramChannelConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliChannelConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for CliChannelConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelegramChannelConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bot_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsConfig {
    pub router: RouterModelConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterModelConfig {
    #[serde(default = "default_ollama")]
    pub provider: String,
    #[serde(default = "default_router_model")]
    pub model: String,
    #[serde(default = "default_ollama_url")]
    pub base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainConfig {
    #[serde(default = "default_python_cmd")]
    pub python_command: String,
    #[serde(default = "default_brain_module")]
    pub module: String,
    #[serde(default = "default_restart_max")]
    pub restart_max: u32,
}

impl Default for BrainConfig {
    fn default() -> Self {
        Self {
            python_command: "uv run python".to_string(),
            module: "nerv".to_string(),
            restart_max: 3,
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_ollama() -> String {
    "ollama".to_string()
}
fn default_router_model() -> String {
    "qwen2.5:3b".to_string()
}
fn default_ollama_url() -> String {
    "http://localhost:11434".to_string()
}
fn default_log_level() -> String {
    "info".to_string()
}
fn default_python_cmd() -> String {
    "uv run python".to_string()
}
fn default_brain_module() -> String {
    "nerv".to_string()
}
fn default_restart_max() -> u32 {
    3
}

impl NervConfig {
    /// Load config by merging default config with user overrides.
    ///
    /// Looks for config.yaml next to config.default.yaml (project root).
    /// If config.yaml exists, it takes precedence; otherwise falls back to default.
    pub fn load(project_root: &Path) -> Result<Self, ConfigError> {
        let default_path = project_root.join("core").join("config.default.yaml");
        let user_path = project_root.join("config.yaml");

        // Prefer user config, fall back to default
        let config_path = if user_path.exists() {
            &user_path
        } else {
            &default_path
        };

        let content =
            std::fs::read_to_string(config_path).map_err(|e| ConfigError::ReadFile {
                path: config_path.to_path_buf(),
                source: e,
            })?;

        let config: NervConfig = serde_yaml::from_str(&content)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_config(dir: &Path, filename: &str, content: &str) {
        let core_dir = dir.join("core");
        std::fs::create_dir_all(&core_dir).unwrap();
        let path = if filename == "config.yaml" {
            dir.join(filename)
        } else {
            core_dir.join(filename)
        };
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn test_load_default_config() {
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            "config.default.yaml",
            r#"
channels:
  cli:
    enabled: true
  telegram:
    enabled: false
    bot_token: ""
models:
  router:
    provider: "ollama"
    model: "qwen2.5:3b"
    base_url: "http://localhost:11434"
"#,
        );

        let config = NervConfig::load(dir.path()).unwrap();
        assert!(config.channels.cli.enabled);
        assert!(!config.channels.telegram.enabled);
        assert_eq!(config.models.router.model, "qwen2.5:3b");
    }

    #[test]
    fn test_user_config_overrides_default() {
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            "config.default.yaml",
            r#"
channels:
  cli:
    enabled: true
  telegram:
    enabled: false
    bot_token: ""
models:
  router:
    provider: "ollama"
    model: "qwen2.5:3b"
    base_url: "http://localhost:11434"
"#,
        );
        write_config(
            dir.path(),
            "config.yaml",
            r#"
channels:
  cli:
    enabled: false
  telegram:
    enabled: true
    bot_token: "my-secret-token"
models:
  router:
    provider: "ollama"
    model: "qwen2.5:7b"
    base_url: "http://localhost:11434"
"#,
        );

        let config = NervConfig::load(dir.path()).unwrap();
        assert!(!config.channels.cli.enabled);
        assert!(config.channels.telegram.enabled);
        assert_eq!(config.channels.telegram.bot_token, "my-secret-token");
        assert_eq!(config.models.router.model, "qwen2.5:7b");
    }
}
