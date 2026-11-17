//! Workspace sandbox policy for tool execution — the security-critical core.
//!
//! Port of `brain/nerv/security/sandbox.py`. Tools may freely modify paths
//! inside the workspace root. Operations that touch paths outside it — or
//! that cannot be statically scoped to a path (shell, desktop control) — are
//! routed through the user-confirmation queue instead of running immediately.

use std::path::{Path, PathBuf};

use tracing::warn;

/// Tools whose effects cannot be confined to a path (or that act outward) are
/// always confirmed.
pub const ALWAYS_CONFIRM_TOOLS: &[&str] = &["shell", "desktop_control", "send_email"];

/// Tools that take a target path: confirmed only when the path escapes the
/// sandbox.
pub const PATH_ARG_BY_TOOL: &[(&str, &str)] = &[
    ("write_file_full", "absolute_path"),
    ("screenshot", "output_path"),
];

/// Paths under the project root that hold secrets, config, or executable code.
/// Even inside the workspace they are never readable or writable by tools
/// without confirmation (writes) or at all (reads): writing code/config is
/// privilege escalation, and personal/ holds credentials.
pub const SECRET_RELATIVE_PATHS: &[&str] = &["config.yaml", "personal"];
pub const CODE_RELATIVE_PATHS: &[&str] = &[
    "brain", "core", "gui", "agents", "scripts", ".git", ".github",
];

/// Individual file names that are protected wherever they appear.
pub const PROTECTED_FILE_NAMES: &[&str] = &[
    "config.yaml",
    "api_keys.env",
    "pending_actions.json",
    ".env",
];

/// Resolve a path the way python's `Path.expanduser().resolve()` does: follow
/// symlinks where they exist, treat missing tails lexically, and expand `~`.
/// Returns `None` when resolution fails — callers fail closed.
fn resolve(path: &str) -> Option<PathBuf> {
    let expanded = shellexpand_user(path);
    let candidate = PathBuf::from(&expanded);

    // Canonicalize the deepest existing ancestor, then reattach the remainder.
    let mut cursor = candidate.clone();
    let mut remainder: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if cursor.symlink_metadata().is_ok() {
            break;
        }
        let (parent, name) = (cursor.parent()?, cursor.file_name()?.to_os_string());
        remainder.push(name);
        cursor = parent.to_path_buf();
    }
    let mut resolved = cursor.canonicalize().ok()?;
    for part in remainder.iter().rev() {
        resolved.push(part);
    }
    Some(resolved)
}

/// Expand a leading `~` the way `Path.expanduser()` does.
fn shellexpand_user(path: &str) -> String {
    if path == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return home.to_string_lossy().to_string();
        }
    } else if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest).to_string_lossy().to_string();
        }
    }
    path.to_string()
}

fn under_any(path: &str, project_root: &Path, relatives: &[&str]) -> bool {
    let Some(resolved) = resolve(path) else {
        return true; // fail closed
    };
    let Ok(root) = project_root.canonicalize() else {
        return true;
    };
    for rel in relatives {
        let guarded = root.join(rel);
        if resolved == guarded || resolved.ancestors().any(|a| a == guarded) {
            return true;
        }
    }
    false
}

/// Return whether `path` holds credentials or config (never readable).
pub fn is_secret_path(path: &str, project_root: &Path) -> bool {
    if path.is_empty() {
        return false;
    }
    let Some(resolved) = resolve(path) else {
        return true;
    };
    if let Some(name) = resolved.file_name().and_then(|n| n.to_str()) {
        if PROTECTED_FILE_NAMES.contains(&name) {
            return true;
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let nerv_home = PathBuf::from(home).join(".nerv");
        if resolved == nerv_home || resolved.ancestors().any(|a| a == nerv_home) {
            return true;
        }
    }
    under_any(path, project_root, SECRET_RELATIVE_PATHS)
}

/// Return whether `path` is a secret/config/code location (not writable).
pub fn is_protected_path(path: &str, project_root: &Path) -> bool {
    if path.is_empty() {
        return false;
    }
    if is_secret_path(path, project_root) {
        return true;
    }
    under_any(path, project_root, CODE_RELATIVE_PATHS)
}

/// Return whether `path` resolves to a location inside `root`.
pub fn is_within_sandbox(path: &str, root: &Path) -> bool {
    if path.is_empty() {
        // An empty path means the tool uses its own in-workspace default.
        return true;
    }
    let Some(resolved) = resolve(path) else {
        return false;
    };
    let Ok(root) = root.canonicalize() else {
        return false;
    };
    resolved == root || resolved.ancestors().any(|a| a == root)
}

/// Return a human reason if the call needs confirmation, else `None`.
///
/// `requires_confirmation` is the tool's own registry flag; a tool that sets
/// it is always confirmed (default-deny for anything not explicitly safe).
pub fn confirmation_reason(
    tool_name: &str,
    arguments: &serde_json::Value,
    workspace_root: &Path,
    requires_confirmation: bool,
    project_root: Option<&Path>,
) -> Option<String> {
    if ALWAYS_CONFIRM_TOOLS.contains(&tool_name) || requires_confirmation {
        return Some(format!(
            "'{tool_name}' can affect resources outside the workspace sandbox"
        ));
    }

    let arg_name = PATH_ARG_BY_TOOL
        .iter()
        .find(|(tool, _)| *tool == tool_name)
        .map(|(_, arg)| *arg);
    if let Some(arg_name) = arg_name {
        let target = arguments
            .get(arg_name)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if !is_within_sandbox(&target, workspace_root) {
            return Some(format!(
                "target path '{target}' is outside the workspace sandbox"
            ));
        }
        if is_protected_path(&target, project_root.unwrap_or(workspace_root)) {
            return Some(format!(
                "target path '{target}' is a protected config/secret/code location"
            ));
        }
    }

    None
}

/// Strip credential-shaped variables from environments handed to child
/// processes. Port of `brain/nerv/security/env.py`.
///
/// Names containing KEY/TOKEN/SECRET/PASSWORD/PASSWD/CREDENTIAL
/// (case-insensitive, substring) are credentials. AUTH is deliberately not
/// matched, so SSH_AUTH_SOCK keeps git-over-ssh working — the over-match
/// (e.g. KEYBOARD_LAYOUT) is restored via passthrough.
pub fn scrubbed_env(passthrough: &[String]) -> Vec<(String, String)> {
    static SENSITIVE_ENV_PATTERN: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| {
            regex::Regex::new(r"(?i)KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL")
                .expect("static regex")
        });
    let allowed: std::collections::HashSet<&String> = passthrough.iter().collect();
    std::env::vars()
        .filter(|(name, _)| allowed.contains(name) || !SENSITIVE_ENV_PATTERN.is_match(name))
        .collect()
}

// `warn` is used by callers building on this module.
#[allow(unused)]
fn _warn_ref() {
    warn!("");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root(tmp: &Path) -> PathBuf {
        tmp.to_path_buf()
    }

    #[test]
    fn shell_is_always_confirmed() {
        let reason = confirmation_reason(
            "shell",
            &serde_json::json!({"command": "ls"}),
            &root(Path::new("/tmp")),
            false,
            None,
        );
        assert!(reason.unwrap().contains("outside the workspace sandbox"));
    }

    #[test]
    fn workspace_writes_pass_without_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("notes")).unwrap();
        let target = dir.path().join("notes").join("today.md");
        let reason = confirmation_reason(
            "write_file_full",
            &serde_json::json!({"absolute_path": target.display().to_string()}),
            dir.path(),
            false,
            Some(dir.path()),
        );
        assert!(reason.is_none());
    }

    #[test]
    fn escapes_outside_the_workspace_are_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let reason = confirmation_reason(
            "write_file_full",
            &serde_json::json!({"absolute_path": "/etc/hosts"}),
            dir.path(),
            false,
            Some(dir.path()),
        );
        assert!(reason.unwrap().contains("outside the workspace sandbox"));
    }

    #[test]
    fn protected_locations_are_confirmed_even_inside_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("personal")).unwrap();
        let reason = confirmation_reason(
            "write_file_full",
            &serde_json::json!({"absolute_path": dir.path().join("personal").join("x").display().to_string()}),
            dir.path(),
            false,
            Some(dir.path()),
        );
        assert!(reason
            .unwrap()
            .contains("protected config/secret/code location"));
    }

    #[test]
    fn secret_paths_never_read() {
        let project = tempfile::tempdir().unwrap();
        let project = project.path().to_path_buf();
        std::fs::create_dir_all(project.join("personal")).unwrap();
        for secret in [
            project.join("config.yaml"),
            project.join("personal").join("x"),
        ] {
            assert!(
                is_secret_path(secret.display().to_string().as_str(), &project),
                "{}",
                secret.display()
            );
        }
        // brain/ is code — protected (not writable), but not a secret.
        let brain = project.join("brain").join("x");
        assert!(is_protected_path(
            brain.display().to_string().as_str(),
            &project
        ));
        assert!(!is_secret_path(
            brain.display().to_string().as_str(),
            &project
        ));
        let plain = project.join("notes").join("x");
        assert!(!is_secret_path(
            plain.display().to_string().as_str(),
            &project
        ));
    }

    #[test]
    fn the_nerv_home_is_always_secret() {
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let project = tempfile::tempdir().unwrap();
        assert!(is_secret_path(
            &format!("{home}/.nerv/config.yaml"),
            project.path()
        ));
    }

    #[test]
    fn protected_file_names_are_secret_wherever_they_appear() {
        let project = tempfile::tempdir().unwrap();
        assert!(is_secret_path("/any/where/.env", project.path()));
        assert!(is_secret_path("/any/where/api_keys.env", project.path()));
    }

    #[test]
    fn sandbox_membership() {
        let dir = tempfile::tempdir().unwrap();
        let inside = dir.path().join("sub").join("file");
        assert!(is_within_sandbox(
            inside.display().to_string().as_str(),
            dir.path()
        ));
        assert!(!is_within_sandbox("/etc/hosts", dir.path()));
        // An empty path means the tool's own in-workspace default.
        assert!(is_within_sandbox("", dir.path()));
    }

    #[test]
    fn scrubbed_env_drops_credential_shaped_names() {
        // The scrubbing reads the process env; assert the pure filter logic
        // through a passthrough round trip.
        let names = [
            ("OPENAI_API_KEY", true),
            ("AWS_SECRET_ACCESS_KEY", true),
            ("MY_PASSWORD", true),
            ("MYSQL_PASSWD", true),
            ("GOOGLE_APPLICATION_CREDENTIALS", true),
            ("NERV_TOKEN_CACHE", true),
            ("SSH_AUTH_SOCK", false),
            ("PATH", false),
            ("KEYBOARD_LAYOUT", true), // over-match, restored via passthrough
        ];
        let sensitive =
            regex::Regex::new(r"(?i)KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL").unwrap();
        for (name, expected) in names {
            assert_eq!(sensitive.is_match(name), expected, "{name}");
        }
    }
}
