//! Config-driven MCP provider manager.
//!
//! Port of `brain/nerv/mcp/manager.py`: owns the configured MCP server
//! processes, resolves capability bindings, and formats tool results.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use tracing::{debug, warn};

use super::client::{McpError, McpServerConfig, StdioMcpClient, DEFAULT_MCP_PROTOCOL_VERSION};
use super::presets::{get_builtin_mcp_preset, merge_preset_into_server_config};

/// One server's status for the `mcp_status` tool.
#[derive(Debug, Clone, Default)]
pub struct McpStatusEntry {
    pub name: String,
    pub preset: String,
    pub enabled: bool,
    pub transport: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub capabilities: Vec<String>,
    pub action_map: Vec<(String, String)>,
    pub healthy: bool,
    pub available_tools: Vec<String>,
    pub error: String,
}

impl McpStatusEntry {
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "preset": self.preset,
            "enabled": self.enabled,
            "transport": self.transport,
            "command": self.command,
            "args": self.args,
            "cwd": self.cwd,
            "capabilities": self.capabilities,
            "action_map": self.action_map.iter().cloned().collect::<HashMap<_, _>>(),
            "healthy": self.healthy,
            "available_tools": self.available_tools,
            "error": self.error,
        })
    }
}

/// A formatted MCP tool result (`content` + `is_error`).
#[derive(Debug, Clone)]
pub struct McpToolResult {
    pub content: String,
    pub is_error: bool,
}

/// Owns configured MCP servers and capability bindings.
pub struct McpServerManager {
    project_root: PathBuf,
    protocol_version: String,
    server_configs: Vec<McpServerConfig>,
    clients: HashMap<String, StdioMcpClient>,
    tool_cache: HashMap<String, Vec<String>>,
}

impl McpServerManager {
    /// Load and normalize the MCP config from the merged project config.
    pub fn new(project_root: &Path) -> Self {
        let config = nerv_shared::config::NervConfig::load(project_root)
            .map(|c| c.mcp)
            .unwrap_or(Value::Null);
        Self::from_config_value(project_root, &config)
    }

    /// The manager from a raw `mcp` config value (tests inject configs here).
    pub fn from_config_value(project_root: &Path, mcp_config: &Value) -> Self {
        let mut server_configs = Vec::new();
        let mut protocol_version = DEFAULT_MCP_PROTOCOL_VERSION.to_string();
        if let Some(version) = mcp_config.get("protocol_version").and_then(|v| v.as_str()) {
            if !version.trim().is_empty() {
                protocol_version = version.trim().to_string();
            }
        }
        if let Some(servers) = mcp_config.get("servers").and_then(|v| v.as_object()) {
            for (name, raw_value) in servers {
                if raw_value.is_object() {
                    server_configs.push(merge_preset_into_server_config(name, raw_value));
                }
            }
        }
        Self {
            project_root: project_root.to_path_buf(),
            protocol_version,
            server_configs,
            clients: HashMap::new(),
            tool_cache: HashMap::new(),
        }
    }

    /// Whether any enabled MCP server is bound to the capability.
    pub fn has_capability_binding(&self, capability_name: &str) -> bool {
        self.find_server_for_capability(capability_name).is_some()
    }

    /// Collect status information for one or all configured MCP servers.
    pub async fn get_status(&mut self, server_name: &str) -> Result<Vec<McpStatusEntry>, McpError> {
        let normalized = server_name.trim();
        if !normalized.is_empty() {
            let config = self
                .server_configs
                .iter()
                .find(|c| c.name == normalized)
                .cloned()
                .ok_or_else(|| McpError::Transport(format!("Unknown MCP server: {server_name}")))?;
            return Ok(vec![self.build_status_entry(&config).await]);
        }
        let configs: Vec<McpServerConfig> = self.server_configs.clone();
        let mut entries = Vec::new();
        for config in &configs {
            entries.push(self.build_status_entry(config).await);
        }
        Ok(entries)
    }

    /// Invoke an MCP-bound tool for a logical capability action.
    ///
    /// `None` means no MCP server handles the capability (or its action), so
    /// the caller falls back to the native tool.
    pub async fn invoke_capability_action(
        &mut self,
        capability_name: &str,
        action: &str,
        arguments: &Value,
    ) -> Result<Option<McpToolResult>, McpError> {
        let Some(config) = self
            .server_configs
            .iter()
            .find(|c| c.enabled && c.capabilities.iter().any(|c| c == capability_name))
            .cloned()
        else {
            return Ok(None);
        };

        let Some(tool_name) = config.action_map.get(action).cloned() else {
            debug!(
                "No MCP action map for capability {capability_name} action {action} on server {}",
                config.name
            );
            return Ok(None);
        };

        let mut request_arguments = arguments.clone();
        if let Some(preset) = get_builtin_mcp_preset(&config.preset) {
            if let Some(binding) = preset
                .action_bindings
                .iter()
                .find(|b| b.tool_name == tool_name)
            {
                request_arguments = (binding.build_arguments)(arguments);
            }
        }

        let server_name = config.name.clone();
        let cached = self.tool_cache.get(&server_name).cloned();
        let tool_names = match cached {
            Some(cached) => cached,
            None => {
                let client = self.get_client(&config).await?;
                let names = client.tool_names().await?;
                self.tool_cache.insert(server_name.clone(), names.clone());
                names
            }
        };
        if !tool_names.contains(&tool_name) {
            warn!(
                "Configured MCP tool {tool_name} is unavailable on server {server_name}; using fallback."
            );
            return Ok(None);
        }

        let client = self.clients.get_mut(&server_name).ok_or_else(|| {
            McpError::Transport(format!("MCP server {server_name} is not running."))
        })?;
        let result = client.call_tool(&tool_name, &request_arguments).await?;
        Ok(Some(Self::format_tool_result(
            &server_name,
            &tool_name,
            &result,
        )))
    }

    /// Close all active MCP clients (intended for shutdown and tests).
    pub async fn close_all(&mut self) {
        for (_, mut client) in self.clients.drain() {
            client.close().await;
        }
        self.tool_cache.clear();
    }

    /// Build a structured status snapshot for one configured server.
    async fn build_status_entry(&mut self, config: &McpServerConfig) -> McpStatusEntry {
        let mut entry = McpStatusEntry {
            name: config.name.clone(),
            preset: config.preset.clone(),
            enabled: config.enabled,
            transport: config.transport.clone(),
            command: config.command.clone(),
            args: config.args.clone(),
            cwd: config.cwd.clone(),
            capabilities: config.capabilities.clone(),
            action_map: config
                .action_map
                .iter()
                .map(|(a, t)| (a.clone(), t.clone()))
                .collect(),
            healthy: false,
            available_tools: Vec::new(),
            error: String::new(),
        };

        if !config.enabled {
            entry.error = "Server is disabled.".to_string();
            return entry;
        }
        if config.command.is_empty() {
            entry.error = "Server is enabled but command is empty.".to_string();
            return entry;
        }

        if let Err(e) = self.get_client(config).await {
            entry.error = e.to_string();
            return entry;
        }
        match self.get_tool_names(&config.name).await {
            Ok(mut tool_names) => {
                tool_names.sort();
                entry.healthy = true;
                entry.available_tools = tool_names;
            }
            Err(e) => entry.error = e.to_string(),
        }
        entry
    }

    fn find_server_for_capability(&self, capability_name: &str) -> Option<&McpServerConfig> {
        self.server_configs.iter().find(|config| {
            config.enabled && config.capabilities.iter().any(|c| c == capability_name)
        })
    }

    async fn get_client(
        &mut self,
        config: &McpServerConfig,
    ) -> Result<&mut StdioMcpClient, McpError> {
        if !self.clients.contains_key(&config.name) {
            self.clients.insert(
                config.name.clone(),
                StdioMcpClient::new(config.clone(), &self.project_root, &self.protocol_version),
            );
        }
        let client = self.clients.get_mut(&config.name).expect("just inserted");
        client.connect().await?;
        Ok(client)
    }

    /// Fetch and cache tool names for a server.
    async fn get_tool_names(&mut self, server_name: &str) -> Result<Vec<String>, McpError> {
        if let Some(cached) = self.tool_cache.get(server_name) {
            return Ok(cached.clone());
        }
        let client = self.clients.get_mut(server_name).ok_or_else(|| {
            McpError::Transport(format!("MCP server {server_name} is not running."))
        })?;
        let tool_names = client.tool_names().await?;
        self.tool_cache
            .insert(server_name.to_string(), tool_names.clone());
        Ok(tool_names)
    }

    /// Convert an MCP CallToolResult payload into a Nerv tool result.
    fn format_tool_result(server_name: &str, tool_name: &str, result: &Value) -> McpToolResult {
        let is_error = result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut lines: Vec<String> = Vec::new();

        let structured = result.get("structuredContent");
        if structured.is_some_and(|v| !v.is_null() && !v.is_object())
            || structured.is_some_and(|v| v.as_object().is_some_and(|o| !o.is_empty()))
        {
            if let Some(v) = structured {
                lines.push(serde_json::to_string(v).unwrap_or_default());
            }
        }

        if let Some(content) = result.get("content").and_then(|v| v.as_array()) {
            for item in content {
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    if item.get("type").and_then(|v| v.as_str()) == Some("text") {
                        lines.push(text.to_string());
                    } else {
                        lines.push(serde_json::to_string(item).unwrap_or_default());
                    }
                } else {
                    lines.push(item.to_string());
                }
            }
        }

        if lines.is_empty() {
            lines.push(format!(
                "MCP tool {tool_name} on {server_name} returned no content."
            ));
        }

        McpToolResult {
            content: lines
                .into_iter()
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            is_error,
        }
    }
}

/// Cached managers per project root (python used a dict + lock; the resolved
/// root is the key, so tests with tempdirs never collide).
static MANAGERS: OnceLock<
    std::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<McpServerManager>>>>,
> = OnceLock::new();

/// The process-wide MCP manager for the project root. MCP server processes
/// are long-lived and shared across the turn loop and the MCP server binary.
pub fn get_mcp_manager(project_root: &Path) -> Arc<tokio::sync::Mutex<McpServerManager>> {
    let resolved = project_root.to_path_buf();
    let map = MANAGERS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut map = map.lock().unwrap();
    map.entry(resolved)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(McpServerManager::new(project_root))))
        .clone()
}
