//! Shared plumbing for the builtin tools: path resolution, output truncation
//! with spill files. Port of `brain/nerv/tools/_helpers.py`.

use std::path::{Path, PathBuf};

/// Timezone aliases for `current_time` (python `TIMEZONE_ALIASES`).
pub fn timezone_alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "utc" | "gmt" => "UTC",
        "seattle" | "los angeles" | "san francisco" => "America/Los_Angeles",
        "new york" => "America/New_York",
        "beijing" | "shanghai" => "Asia/Shanghai",
        "tokyo" => "Asia/Tokyo",
        "london" => "Europe/London",
        _ => return None,
    })
}

pub const MAX_TOOL_OUTPUT_CHARS: usize = 5000;
const SPILL_PREVIEW_HEAD_CHARS: usize = 3000;
const SPILL_PREVIEW_TAIL_CHARS: usize = 1000;
const SPILL_RETENTION_DAYS: u64 = 7;
const SPILL_DIR_ENV: &str = "NERV_SPILL_DIR";
const SPILL_FILE_PREFIX: &str = "spill-";
const SPILL_FILE_SUFFIX: &str = ".txt";

/// Resolve a path for tool access: absolute stays, relative joins the root.
pub fn resolve_tool_path(path: &str, project_root: &Path) -> PathBuf {
    let expanded = expanduser(path);
    let candidate = PathBuf::from(&expanded);
    if candidate.is_absolute() {
        candidate
    } else {
        project_root.join(candidate)
    }
}

fn expanduser(path: &str) -> String {
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

/// Directory for spill files: readable by `read_file`, never a secret path.
/// `personal/` is a secret path for the file tools, so spill files live in a
/// per-user directory under the system temp dir instead; `NERV_SPILL_DIR`
/// overrides it.
fn spill_dir() -> PathBuf {
    if let Ok(override_dir) = std::env::var(SPILL_DIR_ENV) {
        let trimmed = override_dir.trim();
        if !trimmed.is_empty() {
            return expanduser(trimmed).into();
        }
    }
    let uid = users_uid();
    std::env::temp_dir().join(format!("nerv-spill-{uid}"))
}

fn users_uid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Create `directory` (0700) and verify it is ours and not a symlink.
fn prepare_spill_dir(directory: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let info = std::fs::symlink_metadata(directory)?;
        if !info.file_type().is_dir() || info.uid() != users_uid() {
            return Err(std::io::Error::other(
                "spill directory is not a private directory owned by us",
            ));
        }
        if info.mode() & 0o077 != 0 {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// Delete spill files older than the retention window (own naming only).
fn cleanup_spill_files(directory: &Path, now: f64) {
    let cutoff = now - (SPILL_RETENTION_DAYS * 86400) as f64;
    if let Ok(entries) = std::fs::read_dir(directory) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !(name.starts_with(SPILL_FILE_PREFIX) && name.ends_with(SPILL_FILE_SUFFIX)) {
                continue;
            }
            if let Ok(info) = entry.metadata() {
                let mtime = info
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                if mtime < cutoff {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

/// Write the full text to a fresh 0600 file and return its path.
fn spill_to_file(text: &str, tool_name: &str) -> std::io::Result<PathBuf> {
    let directory = spill_dir();
    prepare_spill_dir(&directory)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    cleanup_spill_files(&directory, now);
    let safe_tool: String = tool_name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .take(40)
        .collect();
    let safe_tool = if safe_tool.is_empty() {
        "tool".to_string()
    } else {
        safe_tool
    };
    let stamp = chrono::DateTime::from_timestamp(now as i64, 0)
        .map(|dt| dt.format("%Y%m%dT%H%M%S").to_string())
        .unwrap_or_default();
    let token: String = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let target = directory.join(format!(
        "{SPILL_FILE_PREFIX}{safe_tool}-{stamp}-{token}{SPILL_FILE_SUFFIX}"
    ));
    use std::io::Write;
    let mut file = open_exclusive(&target)?;
    file.write_all(text.as_bytes())?;
    Ok(target)
}

fn open_exclusive(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// Shrink large tool output for context safety, spilling the full text.
///
/// Over the limit, the full text is saved to a private file and the caller
/// gets a head and tail preview plus the omitted size and the file path. If
/// the file cannot be written the preview is still returned, with a note
/// saying so.
pub fn truncate(text: &str, limit: usize, tool_name: &str) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let head_len = SPILL_PREVIEW_HEAD_CHARS.min(limit * 3 / 4);
    let tail_len = SPILL_PREVIEW_TAIL_CHARS.min(limit.saturating_sub(head_len));
    let omitted = text.len() - head_len - tail_len;
    let head: String = text.chars().take(head_len).collect();
    let tail: String = if tail_len > 0 {
        text.chars().skip(text.chars().count() - tail_len).collect()
    } else {
        String::new()
    };
    let note = match spill_to_file(text, tool_name) {
        Ok(spill_path) => format!(
            "[{omitted} characters omitted. Full output ({} characters) saved to {} - use read_file to retrieve it]",
            text.len(),
            spill_path.display()
        ),
        Err(exc) => {
            tracing::warn!("Could not spill tool output: {exc}");
            format!("[{omitted} characters omitted; saving the full output failed]")
        }
    };
    format!("{head}\n...\n{note}\n...\n{tail}")
}

/// Parse comma- or newline-separated text into a clean string list.
#[allow(dead_code)]
pub fn parse_csv_or_lines(value: &str) -> Vec<String> {
    if value.trim().is_empty() {
        return Vec::new();
    }
    value
        .replace(['\r', ','], "\n")
        .split('\n')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

/// Normalize a user URL into an absolute browser URL.
#[allow(dead_code)]
pub fn ensure_url(target_url: &str) -> String {
    let normalized = target_url.trim().to_string();
    if normalized.is_empty() {
        return normalized;
    }
    let has_scheme = normalized.contains("://");
    if has_scheme {
        normalized
    } else {
        format!("https://{normalized}")
    }
}
