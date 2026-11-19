//! Tests for the MCP manager/client, ported from `brain/tests/test_mcp.py`
//! and `test_mcp_timeout.py`. Fake MCP servers are `/bin/sh` JSON-RPC scripts
//! (the fake-brain test technique).

use std::path::Path;
use std::sync::Arc;

use super::client::McpServerConfig;
use super::manager::McpServerManager;
use serde_json::json;

/// Write the project config files the loader needs, with the strict file
/// mode the config loader enforces.
fn write_project_config(tmp: &Path, user_config: &str) {
    std::fs::create_dir_all(tmp.join("core")).unwrap();
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("config.default.yaml"),
        tmp.join("core/config.default.yaml"),
    )
    .unwrap();
    std::fs::write(tmp.join("config.yaml"), user_config).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            tmp.join("config.yaml"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
}

/// A minimal fake MCP server: initialize, tools/list with `name`, and a
/// tools/call that echoes the URL.
fn fake_server(tool_name: &str, reply_prefix: &str) -> String {
    format!(
        r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":[[:space:]]*\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":"2024-11-05"}}}}\n' "$id" ;;
    *'"method":"notifications/initialized"'*) ;;
    *'"method":"tools/list"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"tools":[{{"name":"%s"}}]}}}}\n' "$id" "{tool}" ;;
    *'"method":"tools/call"'*)
      url=$(printf '%s' "$line" | sed -n 's/.*"url":[[:space:]]*"\([^"]*\)".*/\1/p')
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"content":[{{"type":"text","text":"%s %s"}}]}}}}\n' "$id" "{reply}" "$url" ;;
  esac
done
"#,
        tool = tool_name,
        reply = reply_prefix
    )
}

#[test]
fn manager_detects_capability_binding() {
    let tmp = tempfile::tempdir().unwrap();
    write_project_config(
        tmp.path(),
        "mcp:\n  servers:\n    chrome:\n      enabled: true\n      command: \"/bin/sh\"\n      capabilities: [\"browser.interactive\"]\n",
    );
    let manager = McpServerManager::new(tmp.path());
    assert!(manager.has_capability_binding("browser.interactive"));
    assert!(!manager.has_capability_binding("browser.read"));
}

#[tokio::test]
async fn status_reports_disabled_server_without_launching_it() {
    let tmp = tempfile::tempdir().unwrap();
    write_project_config(
        tmp.path(),
        "mcp:\n  servers:\n    chrome:\n      enabled: false\n      command: \"/bin/sh\"\n",
    );
    let mut manager = McpServerManager::new(tmp.path());
    let statuses = manager.get_status("").await.unwrap();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].name, "chrome");
    assert!(!statuses[0].enabled);
    assert!(!statuses[0].healthy);
    assert!(statuses[0].error.to_lowercase().contains("disabled"));
}

#[tokio::test]
async fn browser_interactive_prefers_the_mcp_server() {
    let tmp = tempfile::tempdir().unwrap();
    write_project_config(
        tmp.path(),
        &format!(
            "mcp:\n  servers:\n    chrome:\n      enabled: true\n      command: \"/bin/sh\"\n      args: [\"{script}\"]\n      capabilities: [\"browser.interactive\"]\n      action_map:\n        open_url: \"open_url\"\n",
            script = tmp.path().join("fake_mcp_server.sh").display()
        ),
    );
    std::fs::write(
        tmp.path().join("fake_mcp_server.sh"),
        fake_server("open_url", "MCP opened"),
    )
    .unwrap();
    let mut manager = McpServerManager::new(tmp.path());

    let result = manager
        .invoke_capability_action(
            "browser.interactive",
            "open_url",
            &json!({"url": "https://example.com"}),
        )
        .await
        .unwrap()
        .expect("the MCP provider handled the action");
    assert!(!result.is_error);
    assert!(
        result.content.contains("MCP opened https://example.com"),
        "{}",
        result.content
    );
}

#[tokio::test]
async fn preset_action_binding_translates_nerv_actions() {
    let tmp = tempfile::tempdir().unwrap();
    write_project_config(
        tmp.path(),
        &format!(
            "mcp:\n  servers:\n    chrome:\n      enabled: true\n      preset: \"chrome_devtools_official\"\n      command: \"/bin/sh\"\n      args: [\"{script}\"]\n",
            script = tmp.path().join("fake_mcp_server.sh").display()
        ),
    );
    // The preset maps open_url -> new_page; the fake server exposes it.
    std::fs::write(
        tmp.path().join("fake_mcp_server.sh"),
        fake_server("new_page", "new_page"),
    )
    .unwrap();
    let mut manager = McpServerManager::new(tmp.path());

    let result = manager
        .invoke_capability_action(
            "browser.interactive",
            "open_url",
            &json!({"url": "https://preset.example"}),
        )
        .await
        .unwrap()
        .expect("the preset-backed provider handled the action");
    assert!(!result.is_error);
    assert!(
        result.content.contains("new_page https://preset.example"),
        "{}",
        result.content
    );
}

#[tokio::test]
async fn missing_mcp_tool_falls_back_to_none() {
    let tmp = tempfile::tempdir().unwrap();
    write_project_config(
        tmp.path(),
        &format!(
            "mcp:\n  servers:\n    chrome:\n      enabled: true\n      command: \"/bin/sh\"\n      args: [\"{script}\"]\n      capabilities: [\"browser.interactive\"]\n      action_map:\n        open_url: \"open_url\"\n",
            script = tmp.path().join("fake_mcp_server.sh").display()
        ),
    );
    // The server only exposes a different tool: the configured one is
    // unavailable, so the manager reports None (native fallback runs).
    std::fs::write(
        tmp.path().join("fake_mcp_server.sh"),
        fake_server("other_tool", "unused"),
    )
    .unwrap();
    let mut manager = McpServerManager::new(tmp.path());

    let result = manager
        .invoke_capability_action(
            "browser.interactive",
            "open_url",
            &json!({"url": "https://example.com"}),
        )
        .await
        .unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn timeout_error_names_server_method_and_limit() {
    let tmp = tempfile::tempdir().unwrap();
    // A server that never answers.
    std::fs::write(
        tmp.path().join("fake_mcp_server.sh"),
        "#!/bin/sh\nwhile IFS= read -r line; do :; done\n",
    )
    .unwrap();
    let config = McpServerConfig {
        name: "slow".to_string(),
        enabled: true,
        transport: "stdio".to_string(),
        command: "/bin/sh".to_string(),
        args: vec![tmp.path().join("fake_mcp_server.sh").display().to_string()],
        request_timeout: 0.2,
        ..Default::default()
    };
    let config_value = serde_json::json!({"servers": {"slow": {
        "enabled": true, "transport": "stdio",
        "command": "/bin/sh",
        "args": [tmp.path().join("fake_mcp_server.sh").display().to_string()],
        "request_timeout": 0.2
    }}});
    let mut manager = McpServerManager::from_config_value(tmp.path(), &config_value);
    let _ = config;

    let err = manager
        .get_status("slow")
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .error;
    assert!(
        err.contains("slow") && err.contains("timed out") && err.contains("0.2"),
        "{err}"
    );
}

#[tokio::test]
async fn mcp_status_reports_remote_tools_for_a_healthy_server() {
    let tmp = tempfile::tempdir().unwrap();
    write_project_config(
        tmp.path(),
        &format!(
            "mcp:\n  servers:\n    chrome:\n      enabled: true\n      command: \"/bin/sh\"\n      args: [\"{script}\"]\n      capabilities: [\"browser.interactive\"]\n",
            script = tmp.path().join("fake_mcp_server.sh").display()
        ),
    );
    std::fs::write(
        tmp.path().join("fake_mcp_server.sh"),
        fake_server("open_url", "unused"),
    )
    .unwrap();
    let mut manager = McpServerManager::new(tmp.path());
    let statuses = manager.get_status("").await.unwrap();
    assert_eq!(statuses.len(), 1);
    assert!(statuses[0].healthy);
    assert_eq!(statuses[0].available_tools, vec!["open_url"]);
}

#[tokio::test]
async fn the_global_manager_is_cached_per_project_root() {
    let tmp = tempfile::tempdir().unwrap();
    let a = super::get_mcp_manager(tmp.path());
    let b = super::get_mcp_manager(tmp.path());
    assert!(Arc::ptr_eq(&a, &b), "same root returns the same manager");
    let other = super::get_mcp_manager(tmp.path().join("other").as_path());
    assert!(
        !Arc::ptr_eq(&a, &other),
        "different roots get their own manager"
    );
}
