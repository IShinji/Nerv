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
    #[error("invalid config: {0}")]
    Invalid(String),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Only accept messages from private (1:1) chats. Group chats are refused
    /// unless this is turned off explicitly.
    #[serde(default = "default_true")]
    pub private_chats_only: bool,
    /// Directories (relative to the project root, or absolute) whose files the
    /// bot may upload through a `[DOCUMENT: path]` marker. Nothing outside
    /// them is ever sent, and dotfiles / `config.yaml` are always refused.
    #[serde(default = "default_document_dirs")]
    pub document_dirs: Vec<String>,
}

impl Default for TelegramChannelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: String::new(),
            allowed_users: Vec::new(),
            private_chats_only: true,
            document_dirs: default_document_dirs(),
        }
    }
}

fn default_document_dirs() -> Vec<String> {
    vec!["personal/workspace".to_string()]
}

impl TelegramChannelConfig {
    /// Whether `sender` (a Telegram user id) may talk to this instance.
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
    /// The user config is, in order of precedence: the file named by the
    /// `NERV_CONFIG` environment variable (it must exist), `config.yaml` in
    /// the project root, then `~/.nerv/config.yaml`. When present, it overrides
    /// only the keys it defines.
    pub fn load(project_root: &Path) -> Result<Self, ConfigError> {
        let allow_custom =
            std::env::var("NERV_ALLOW_CUSTOM_PYTHON_COMMAND").is_ok_and(|v| v == "1");
        Self::load_with_options(project_root, allow_custom)
    }

    /// Like [`NervConfig::load`], with the custom-launcher opt-in passed explicitly.
    pub fn load_with_options(
        project_root: &Path,
        allow_custom_python_command: bool,
    ) -> Result<Self, ConfigError> {
        let default_path = project_root.join("core").join("config.default.yaml");
        let user_path = resolve_user_config_path(
            project_root,
            std::env::var_os("NERV_CONFIG").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        );

        let default_content =
            std::fs::read_to_string(&default_path).map_err(|e| ConfigError::ReadFile {
                path: default_path.clone(),
                source: e,
            })?;
        let mut merged: serde_yaml::Value = serde_yaml::from_str(&default_content)?;

        if let Some(user_path) = user_path {
            let user_content =
                std::fs::read_to_string(&user_path).map_err(|e| ConfigError::ReadFile {
                    path: user_path.clone(),
                    source: e,
                })?;
            let user_value: serde_yaml::Value = serde_yaml::from_str(&user_content)?;
            merge_yaml(&mut merged, user_value);
        }

        let config: NervConfig = serde_yaml::from_value(merged)?;
        validate_python_command(&config.brain.python_command, allow_custom_python_command)
            .map_err(ConfigError::Invalid)?;
        Ok(config)
    }

    /// Non-fatal findings the host should log once at startup: an unfamiliar
    /// `brain.python_command` launcher and an over-permissive user config file.
    pub fn startup_warnings(&self, project_root: &Path) -> Vec<String> {
        let mut warnings = Vec::new();
        if let Some(w) = python_command_warning(&self.brain.python_command) {
            warnings.push(w);
        }
        let user_path = resolve_user_config_path(
            project_root,
            std::env::var_os("NERV_CONFIG").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        );
        #[cfg(unix)]
        if let Some(path) = user_path {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&path) {
                if let Some(w) = file_mode_warning(&path, meta.permissions().mode()) {
                    warnings.push(w);
                }
            }
        }
        #[cfg(not(unix))]
        let _ = user_path;
        warnings
    }
}

/// Reject a `brain.python_command` that cannot name a program, or whose
/// launcher is not a known one unless the operator opted in. The gateway runs
/// this string as a program, so anyone who can edit the config file could
/// otherwise run anything; the opt-in lives in the environment, which a file
/// write cannot change.
pub fn validate_python_command(command: &str, allow_custom: bool) -> Result<(), String> {
    if command.trim().is_empty() {
        return Err("brain.python_command must not be empty".to_string());
    }
    if !allow_custom && !is_known_launcher(command) {
        return Err(format!(
            "brain.python_command starts with an unrecognized launcher ({command:?}); \
             expected uv, python, python3 or a path to one. \
             Set NERV_ALLOW_CUSTOM_PYTHON_COMMAND=1 in the environment to allow it"
        ));
    }
    Ok(())
}

/// Whether the first token of `command` looks like a known interpreter or
/// launcher: `uv`, `python`, `python3`, or a path ending in `/uv` or `/python*`.
fn is_known_launcher(command: &str) -> bool {
    let Some(first) = command.split_whitespace().next() else {
        return false;
    };
    if first.contains('/') {
        let name = Path::new(first)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        return name == "uv" || name.starts_with("python");
    }
    matches!(first, "uv" | "python" | "python3")
}

/// The gateway executes `brain.python_command` as a program, so an unexpected
/// launcher deserves a visible warning (it is not an error: custom setups exist).
pub fn python_command_warning(command: &str) -> Option<String> {
    if command.trim().is_empty() || is_known_launcher(command) {
        return None;
    }
    Some(format!(
        "brain.python_command starts with an unrecognized launcher ({command:?}); \
         it will be executed as a program. Expected uv, python, python3 or a path to one"
    ))
}

/// Warn when the config file (which can hold a bot token) is accessible to
/// group or others. `mode` is the unix st_mode.
pub fn file_mode_warning(path: &Path, mode: u32) -> Option<String> {
    let bits = mode & 0o077;
    if bits == 0 {
        return None;
    }
    let mut problems = Vec::new();
    if bits & 0o022 != 0 {
        problems.push("writable");
    }
    if bits & 0o044 != 0 {
        problems.push("readable");
    }
    Some(format!(
        "{} is group/world-{} (mode {:o}); it may hold secrets, run: chmod 600 {}",
        path.display(),
        problems.join(" and group/world-"),
        mode & 0o777,
        path.display()
    ))
}

/// Pick the user config file. An explicit override is returned as-is (so a
/// typo fails loudly when it is read); the fallbacks only count if they exist.
fn resolve_user_config_path(
    project_root: &Path,
    env_override: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(path) = env_override.filter(|p| !p.as_os_str().is_empty()) {
        return Some(path);
    }
    let in_project = project_root.join("config.yaml");
    if in_project.exists() {
        return Some(in_project);
    }
    let in_home = home?.join(".nerv").join("config.yaml");
    in_home.exists().then_some(in_home)
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
    fn user_config_path_precedence() {
        let root = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join(".nerv")).unwrap();
        std::fs::write(home.path().join(".nerv/config.yaml"), "").unwrap();
        let home_cfg = home.path().join(".nerv/config.yaml");

        // Only the home fallback exists.
        assert_eq!(
            resolve_user_config_path(root.path(), None, Some(home.path().into())),
            Some(home_cfg.clone())
        );
        // A project-root config wins over the home one.
        std::fs::write(root.path().join("config.yaml"), "").unwrap();
        assert_eq!(
            resolve_user_config_path(root.path(), None, Some(home.path().into())),
            Some(root.path().join("config.yaml"))
        );
        // NERV_CONFIG wins over both, even if missing.
        let custom = root.path().join("nope.yaml");
        assert_eq!(
            resolve_user_config_path(root.path(), Some(custom.clone()), Some(home.path().into())),
            Some(custom)
        );
        // Nothing anywhere.
        let empty = TempDir::new().unwrap();
        assert_eq!(resolve_user_config_path(empty.path(), None, None), None);
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
            ..Default::default()
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

    #[test]
    fn empty_python_command_is_rejected() {
        assert!(validate_python_command("", false).is_err());
        assert!(validate_python_command("  \t ", true).is_err());
        assert!(validate_python_command("uv run python", false).is_ok());
    }

    #[test]
    fn unknown_launcher_is_rejected_unless_opted_in() {
        for cmd in ["bash -c x", "sh", "/tmp/evil", "pythonista"] {
            let err = validate_python_command(cmd, false).unwrap_err();
            assert!(
                err.contains("NERV_ALLOW_CUSTOM_PYTHON_COMMAND"),
                "{cmd}: {err}"
            );
            assert!(validate_python_command(cmd, true).is_ok(), "{cmd}");
        }
        for cmd in ["uv run python", "python3 -u", "./.venv/bin/python"] {
            assert!(validate_python_command(cmd, false).is_ok(), "{cmd}");
        }
    }

    #[test]
    fn load_rejects_unknown_launcher_by_default() {
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            "config.default.yaml",
            "channels: {}\nmodels:\n  router: {}\nbrain:\n  python_command: \"bash -c x\"\n",
        );
        assert!(matches!(
            NervConfig::load_with_options(dir.path(), false),
            Err(ConfigError::Invalid(_))
        ));
        assert!(NervConfig::load_with_options(dir.path(), true).is_ok());
    }

    #[test]
    fn known_launchers_do_not_warn() {
        for cmd in [
            "uv run python",
            "python",
            "python3 -u",
            "/opt/homebrew/bin/uv run python",
            "/usr/bin/python3.12",
            "./.venv/bin/python",
        ] {
            assert!(python_command_warning(cmd).is_none(), "{cmd}");
        }
    }

    #[test]
    fn unknown_launchers_warn() {
        for cmd in ["bash -c x", "/tmp/evil", "pythonista", "sh"] {
            assert!(python_command_warning(cmd).is_some(), "{cmd}");
        }
    }

    #[test]
    fn file_mode_warning_flags_group_and_world_access() {
        let p = Path::new("config.yaml");
        assert!(file_mode_warning(p, 0o100600).is_none());
        assert!(file_mode_warning(p, 0o100644).unwrap().contains("readable"));
        assert!(file_mode_warning(p, 0o100620).unwrap().contains("writable"));
        assert!(file_mode_warning(p, 0o100666).is_some());
    }

    #[test]
    fn load_rejects_blank_python_command() {
        let dir = TempDir::new().unwrap();
        write_config(
            dir.path(),
            "config.default.yaml",
            "channels: {}\nmodels:\n  router: {}\nbrain:\n  python_command: \"  \"\n",
        );
        assert!(matches!(
            NervConfig::load(dir.path()),
            Err(ConfigError::Invalid(_))
        ));
    }
}
