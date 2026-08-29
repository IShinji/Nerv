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
    /// Telegram user/chat ids permitted to talk to this instance.
    ///
    /// The brain can run shell commands and read the filesystem, so an
    /// unrestricted bot is a remote shell for anyone who finds it. This list
    /// therefore fails closed: an empty list authorizes nobody, and the
    /// gateway refuses to start the adapter at all.
    #[serde(default)]
    pub allowed_users: Vec<String>,
}

impl TelegramChannelConfig {
    /// Whether `sender` (a Telegram chat id) may talk to this instance.
    pub fn is_allowed(&self, sender: &str) -> bool {
        let sender = sender.trim();
        !sender.is_empty()
            && self
                .allowed_users
                .iter()
                .any(|allowed| allowed.trim() == sender)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsConfig {
    pub router: RouterModelConfig,
    #[serde(default)]
    pub tiers: TierModelsConfig,
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
pub struct TierModelsConfig {
    #[serde(default = "default_tier0_model")]
    pub tier0: String,
    #[serde(default = "default_tier1_model")]
    pub tier1: String,
    #[serde(default = "default_tier2_model")]
    pub tier2: String,
    #[serde(default = "default_tier3_model")]
    pub tier3: String,
}

impl Default for TierModelsConfig {
    fn default() -> Self {
        Self {
            tier0: default_tier0_model(),
            tier1: default_tier1_model(),
            tier2: default_tier2_model(),
            tier3: default_tier3_model(),
        }
    }
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
    #[serde(default)]
    pub auto_install_ollama: bool,
    /// How many times the gateway may respawn a brain that died.
    #[serde(default = "default_restart_max")]
    pub restart_max: u32,
    /// Per-request ceiling, so a wedged brain surfaces an error instead of
    /// leaving the caller blocked forever.
    #[serde(default = "default_call_timeout_secs")]
    pub call_timeout_secs: u64,
    /// Interval between proactive heartbeat polls.
    #[serde(default = "default_heartbeat_interval_secs")]
    pub heartbeat_interval_secs: u64,
}

impl Default for BrainConfig {
    fn default() -> Self {
        Self {
            python_command: default_python_cmd(),
            module: default_brain_module(),
            auto_install_ollama: false,
            restart_max: default_restart_max(),
            call_timeout_secs: default_call_timeout_secs(),
            heartbeat_interval_secs: default_heartbeat_interval_secs(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_ollama() -> String {
    "ollama".to_string()
}
// These fall back to the same values as `core/config.default.yaml` and the
// Python brain's `DEFAULT_TIER_MODELS`. Keeping the three in sync matters: a
// config that omits a key must not silently give the Rust and Python halves
// different models.
fn default_router_model() -> String {
    "claude-cli:opus".to_string()
}
fn default_tier0_model() -> String {
    "claude-cli:opus".to_string()
}
fn default_tier1_model() -> String {
    "claude-cli:opus".to_string()
}
fn default_tier2_model() -> String {
    "claude-cli:opus".to_string()
}
fn default_tier3_model() -> String {
    "claude-cli:opus".to_string()
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
fn default_call_timeout_secs() -> u64 {
    300
}
fn default_heartbeat_interval_secs() -> u64 {
    30
}

impl NervConfig {
    /// Load config by merging default config with user overrides.
    ///
    /// Looks for config.yaml next to config.default.yaml (project root).
    /// When present, user config overrides only the keys it defines.
    pub fn load(project_root: &Path) -> Result<Self, ConfigError> {
        let default_path = project_root.join("core").join("config.default.yaml");
        let user_path = project_root.join("config.yaml");

        let default_content =
            std::fs::read_to_string(&default_path).map_err(|e| ConfigError::ReadFile {
                path: default_path.clone(),
                source: e,
            })?;
        let mut merged: serde_yaml::Value = serde_yaml::from_str(&default_content)?;

        if user_path.exists() {
            let user_content =
                std::fs::read_to_string(&user_path).map_err(|e| ConfigError::ReadFile {
                    path: user_path.clone(),
                    source: e,
                })?;
            let user_value: serde_yaml::Value = serde_yaml::from_str(&user_content)?;
            merge_yaml(&mut merged, user_value);
        }

        let config: NervConfig = serde_yaml::from_value(merged)?;
        Ok(config)
    }
}

fn merge_yaml(base: &mut serde_yaml::Value, override_value: serde_yaml::Value) {
    match (base, override_value) {
        (serde_yaml::Value::Mapping(base_map), serde_yaml::Value::Mapping(override_map)) => {
            for (key, value) in override_map {
                if let Some(base_value) = base_map.get_mut(&key) {
                    merge_yaml(base_value, value);
                } else {
                    base_map.insert(key, value);
                }
            }
        }
        (base_slot, value) => *base_slot = value,
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
    model: "qwen2.5:1.5b"
    base_url: "http://localhost:11434"
"#,
        );

        let config = NervConfig::load(dir.path()).unwrap();
        assert!(config.channels.cli.enabled);
        assert!(!config.channels.telegram.enabled);
        assert_eq!(config.models.router.model, "qwen2.5:1.5b");
        assert_eq!(config.models.tiers.tier0, default_tier0_model());
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
    model: "qwen2.5:1.5b"
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
        assert_eq!(config.models.tiers.tier0, default_tier0_model());
    }

    #[test]
    fn test_partial_user_config_merges_with_default() {
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
    model: "qwen2.5:1.5b"
    base_url: "http://localhost:11434"
logging:
  level: "info"
brain:
  python_command: "uv run python"
  module: "nerv"
  restart_max: 3
"#,
        );
        write_config(
            dir.path(),
            "config.yaml",
            r#"
models:
  router:
    model: "qwen2.5:1.5b"
"#,
        );

        let config = NervConfig::load(dir.path()).unwrap();
        assert!(config.channels.cli.enabled);
        assert!(!config.channels.telegram.enabled);
        assert_eq!(config.models.router.provider, "ollama");
        assert_eq!(config.models.router.model, "qwen2.5:1.5b");
        assert_eq!(config.models.router.base_url, "http://localhost:11434");
        assert_eq!(config.models.tiers.tier0, default_tier0_model());
        assert_eq!(config.brain.module, "nerv");
        // Timing knobs fall back to defaults when the file omits them.
        assert_eq!(config.brain.call_timeout_secs, 300);
        assert_eq!(config.brain.heartbeat_interval_secs, 30);
    }

    #[test]
    fn model_defaults_match_the_shipped_default_config() {
        // core/config.default.yaml is the source of truth; the serde fallbacks
        // exist only for keys a user's config.yaml omits entirely.
        let shipped = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("config.default.yaml"),
        )
        .expect("shipped default config is readable");
        let shipped: NervConfig =
            serde_yaml::from_str(&shipped).expect("shipped default config parses");

        assert_eq!(shipped.models.router.model, default_router_model());
        assert_eq!(shipped.models.tiers.tier0, default_tier0_model());
        assert_eq!(shipped.models.tiers.tier1, default_tier1_model());
        assert_eq!(shipped.models.tiers.tier2, default_tier2_model());
        assert_eq!(shipped.models.tiers.tier3, default_tier3_model());
    }

    #[test]
    fn telegram_allowlist_denies_by_default() {
        let config = TelegramChannelConfig::default();
        assert!(!config.is_allowed("12345"));
        assert!(!config.is_allowed(""));
    }

    #[test]
    fn telegram_allowlist_admits_only_listed_senders() {
        let config = TelegramChannelConfig {
            enabled: true,
            bot_token: "token".to_string(),
            allowed_users: vec!["12345".to_string(), " 67890 ".to_string()],
        };

        assert!(config.is_allowed("12345"));
        assert!(config.is_allowed("67890"), "entries are trimmed");
        assert!(config.is_allowed(" 12345 "), "senders are trimmed");
        assert!(!config.is_allowed("1234"), "no prefix matching");
        assert!(!config.is_allowed("999"));
        assert!(!config.is_allowed(""));
    }

    #[test]
    fn telegram_allowlist_parses_from_yaml() {
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
    allowed_users: []
models:
  router:
    model: "claude-cli:opus"
"#,
        );
        write_config(
            dir.path(),
            "config.yaml",
            r#"
channels:
  telegram:
    enabled: true
    bot_token: "secret"
    allowed_users: ["42"]
"#,
        );

        let config = NervConfig::load(dir.path()).unwrap();
        assert!(config.channels.telegram.is_allowed("42"));
        assert!(!config.channels.telegram.is_allowed("43"));
    }
}
