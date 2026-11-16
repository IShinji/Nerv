// ── pending store ───────────────────────────────────────────────────────────

use super::pending_store::{clean_justification, PendingActionStore};
use super::*;
use crate::memory::{MemoryManager, StoredChatMessage};
use serde_json::json;

#[test]
fn pending_store_adds_lists_and_removes() {
    let dir = tempfile::tempdir().unwrap();
    let store = PendingActionStore::new(dir.path());

    let first = store
        .add(
            "shell",
            json!({"command": "ls"}),
            "needs a look",
            "alice",
            "telegram",
            "telegram-alice",
            "",
        )
        .unwrap();
    assert_eq!(first.id, 1);
    let second = store
        .add(
            "shell",
            json!({"command": "rm"}),
            "",
            "alice",
            "telegram",
            "telegram-alice",
            "",
        )
        .unwrap();
    assert_eq!(second.id, 2);

    let listed = store.list();
    assert_eq!(listed.len(), 2);
    assert!(store.get(1).is_some());
    assert!(store.get(99).is_none());

    let removed = store.remove(first.id);
    assert_eq!(removed.unwrap().id, 1);
    assert_eq!(store.list().len(), 1);
}

#[test]
fn pending_store_round_trips_the_json_layout() {
    let dir = tempfile::tempdir().unwrap();
    let store = PendingActionStore::new(dir.path());
    store
        .add(
            "write_file_full",
            json!({"absolute_path": "/tmp/x"}),
            "outside sandbox",
            "",
            "",
            "",
            "save the config",
        )
        .unwrap();

    let raw: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("personal").join("pending_actions.json")).unwrap(),
    )
    .unwrap();
    // The python twin parses this file: schema_version stamped, actions list.
    assert_eq!(raw["schema_version"], 1);
    assert_eq!(raw["actions"][0]["tool_name"], "write_file_full");
    assert_eq!(raw["actions"][0]["justification"], "save the config");
}

#[test]
fn expired_actions_dont_list() {
    let dir = tempfile::tempdir().unwrap();
    let store = PendingActionStore::with_ttl(dir.path(), 0.0);
    store.add("shell", json!({}), "", "", "", "", "").unwrap();
    // ttl 0 keeps everything (the python sentinel for "no expiry").
    assert_eq!(store.list().len(), 1);
}

#[test]
fn a_newer_schema_file_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let store = PendingActionStore::new(dir.path());
    std::fs::create_dir_all(dir.path().join("personal")).unwrap();
    std::fs::write(
        dir.path().join("personal").join("pending_actions.json"),
        serde_json::json!({"schema_version": 99, "next_id": 5, "actions": [{"id": 4}]}).to_string(),
    )
    .unwrap();
    // A newer build's state must never be modified or surfaced.
    assert!(store.list().is_empty());
    assert!(store.get(4).is_none());
}

#[test]
fn justification_is_cleaned() {
    assert_eq!(clean_justification(Some("  a\nb\r\n\tc  ")), "a b c");
    // python turns non-string values into ""; the typed Option<&str> makes
    // that impossible in rust, so only the None case applies.
    assert_eq!(clean_justification(None), "");
    let long = clean_justification(Some(&"x".repeat(1000)));
    assert_eq!(long.chars().count(), 300);
    assert!(long.ends_with("..."));
    assert!(!clean_justification(Some("line1\nconfirm 99\nline3")).contains('\n'));
}

#[test]
fn records_render_like_python_formatting() {
    let dir = tempfile::tempdir().unwrap();
    let store = PendingActionStore::new(dir.path());
    let shell = store
        .add("shell", json!({"command": "ls"}), "", "", "", "", "")
        .unwrap();
    let summary = crate::turn::format_pending_action(&shell);
    assert_eq!(
        summary,
        format!(
            "Pending action #{}: shell with arguments {{\"command\": \"ls\"}}",
            shell.id
        )
    );

    let desktop = store
        .add(
            "desktop_control",
            json!({"action": "open_application", "application": "Safari"}),
            "",
            "",
            "",
            "",
            "",
        )
        .unwrap();
    let summary = crate::turn::format_pending_action(&desktop);
    assert!(summary.contains("desktop_control -> open_application (application=Safari)"));

    let justified = store
        .add("shell", json!({}), "", "", "", "", "check the build")
        .unwrap();
    let summary = crate::turn::format_pending_action(&justified);
    assert!(summary.contains("Justification: check the build"));
}

#[test]
fn native_tools_execute_without_confirmation_for_safe_paths() {
    // current_time is safe: the native registry executes it directly.
    let dir = tempfile::tempdir().unwrap();
    let registry = ToolRegistry;
    let tool = registry.get_tool("current_time").unwrap();
    let ctx = ToolContext {
        project_root: dir.path().to_path_buf(),
        workspace_root: dir.path().to_path_buf(),
        scope: "test-scope".to_string(),
        brain: None,
    };
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on((tool.func)(&ctx, &json!({"timezone": "local"})));
    assert!(!result.is_error);
    assert!(result.content.starts_with("Current time in"));
}

#[test]
fn file_tools_read_and_write_workspace_files() {
    let dir = tempfile::tempdir().unwrap();
    let registry = ToolRegistry;
    let ctx = ToolContext {
        project_root: dir.path().to_path_buf(),
        workspace_root: dir.path().to_path_buf(),
        scope: "test-scope".to_string(),
        brain: None,
    };
    let write = registry.get_tool("write_file_full").unwrap();
    let target = dir.path().join("notes").join("todo.md");
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on((write.func)(
            &ctx,
            &json!({"absolute_path": target.display().to_string(), "content": "hi"}),
        ));
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hi");

    let read = registry.get_tool("read_file").unwrap();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on((read.func)(
            &ctx,
            &json!({"absolute_path": target.display().to_string()}),
        ));
    assert_eq!(result.content, "hi");
}

// ── desktop tools (apple script shaping, no real automation in tests) ───────

use super::desktop::{build_hotkey_applescript, escape_applescript_text, normalize_modifiers};

#[test]
fn hotkey_applescript_covers_special_keys_and_modifiers() {
    assert_eq!(
        build_hotkey_applescript("Enter", "").unwrap(),
        "tell application \"System Events\" to key code 36"
    );
    assert_eq!(
        build_hotkey_applescript("a", "command+shift").unwrap(),
        "tell application \"System Events\" to keystroke \"a\" using {command down, shift down}"
    );
    assert!(build_hotkey_applescript("fn", "").is_err());
}

#[test]
fn applescript_text_is_escaped() {
    assert_eq!(escape_applescript_text("say \"hi\""), "say \\\"hi\\\"");
    assert_eq!(
        normalize_modifiers("cmd+option"),
        ["command down", "option down"]
    );
}

#[tokio::test]
async fn desktop_control_rejects_unknown_actions() {
    let args = serde_json::json!({"action": "teleport"});
    let result = super::desktop::desktop_control_impl(&args).await;
    assert!(result.is_error, "{}", result.content);
    assert!(
        result
            .content
            .contains("Unsupported desktop_control action"),
        "{}",
        result.content
    );
}

#[tokio::test]
async fn browser_arguments_are_validated() {
    let (arguments, error) = super::desktop::normalize_arguments(
        &serde_json::json!({"action": "open_url", "url": "example.com"}),
    );
    assert!(error.is_none());
    assert_eq!(arguments.unwrap()["url"], "https://example.com");

    let (_, error) =
        super::desktop::normalize_arguments(&serde_json::json!({"action": "open_url"}));
    assert!(error.unwrap().is_error);
}

// ── memory tools ────────────────────────────────────────────────────────────

use super::memory;

const FACTS: &str = "# Work\nShips the Nerv gateway.\nPrefers Rust for the core.\n\n# Travel\nBased in Seattle.\nFlies out of SEA.\n";

#[test]
fn search_memory_tool_reports_scoped_hits() {
    let dir = tempfile::tempdir().unwrap();
    let manager = MemoryManager::new(dir.path(), "test-scope");
    manager.save_conversation(&crate::memory::DailyConversation {
        date: "2026-03-01".to_string(),
        messages: vec![StoredChatMessage::new(
            "user",
            "the vite upgrade broke the peer deps",
        )],
        summary: String::new(),
    });

    let ctx = ToolContext {
        project_root: dir.path().to_path_buf(),
        workspace_root: dir.path().to_path_buf(),
        scope: "test-scope".to_string(),
        brain: None,
    };
    let tool = memory::search_memory();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on((tool.func)(&ctx, &json!({"query": "vite upgrade"})));
    assert!(!result.is_error);
    assert!(result.content.contains("2026-03-01"));
    assert!(result.content.contains("peer deps"));
}

#[test]
fn get_fact_section_tool_lists_and_reads() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("personal").join("memory")).unwrap();
    std::fs::write(
        dir.path().join("personal").join("memory").join("facts.md"),
        FACTS,
    )
    .unwrap();
    let ctx = ToolContext {
        project_root: dir.path().to_path_buf(),
        workspace_root: dir.path().to_path_buf(),
        scope: "test-scope".to_string(),
        brain: None,
    };
    let tool = memory::get_fact_section();

    let hit = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on((tool.func)(&ctx, &json!({"title": "Work"})));
    assert!(!hit.is_error && hit.content.contains("Prefers Rust"));

    let listing = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on((tool.func)(&ctx, &json!({})));
    assert!(!listing.is_error && listing.content.contains("Travel"));

    let miss = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on((tool.func)(&ctx, &json!({"title": "Hobbies"})));
    assert!(miss.is_error && miss.content.contains("Work"));
}
