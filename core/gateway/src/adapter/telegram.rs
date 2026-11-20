use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use nerv_shared::config::TelegramChannelConfig;
use nerv_shared::message::{ChannelType, Message};
use teloxide::prelude::*;
use teloxide::types::Message as TgMessage;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use super::IncomingMessage;

/// Marker Nerv emits to have the gateway upload a file to the channel.
const DOCUMENT_TAG: &str = "[DOCUMENT: ";

/// Telegram adapter — communicates with Telegram Bot API via teloxide.
pub struct TelegramAdapter {
    config: TelegramChannelConfig,
    project_root: PathBuf,
}

impl TelegramAdapter {
    pub fn new(config: TelegramChannelConfig, project_root: PathBuf) -> Self {
        Self {
            config,
            project_root,
        }
    }
}

/// Telegram rejects messages longer than this many characters.
const TELEGRAM_MAX_CHARS: usize = 4096;

/// How often to refresh the "typing..." indicator (it lasts about 5 s).
const TYPING_REFRESH: Duration = Duration::from_secs(4);

/// Minimum spacing between streaming preview edits (Telegram rate limits).
const EDIT_INTERVAL: Duration = Duration::from_secs(1);

/// Split `text` into chunks of at most `max_chars` characters, never inside a
/// character, preferring to break after a newline (then a space) when one
/// falls in the second half of the window.
fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(1);
    let mut chunks = Vec::new();
    let mut rest = text;

    while !rest.is_empty() {
        if rest.chars().count() <= max_chars {
            chunks.push(rest.to_string());
            break;
        }
        // Byte offset just past the `max_chars`-th character.
        let hard = rest
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        let window = &rest[..hard];
        let half = window
            .char_indices()
            .nth(max_chars / 2)
            .map(|(i, _)| i)
            .unwrap_or(0);
        let cut = window
            .rfind('\n')
            .filter(|&i| i >= half)
            .or_else(|| window.rfind(' ').filter(|&i| i >= half))
            .map(|i| i + 1)
            .unwrap_or(hard);

        chunks.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    chunks
}

/// Resolve a path requested through `[DOCUMENT: ...]` to a file that is safe
/// to upload, or `None`.
///
/// Relative paths are resolved against `project_root`. The canonical path (so
/// symlinks and `..` cannot escape) must be a regular file inside one of
/// `allowed_dirs`, with no dotfile component below that directory and not be
/// named `config.yaml`.
fn resolve_document(
    requested: &str,
    project_root: &Path,
    allowed_dirs: &[String],
) -> Option<PathBuf> {
    let requested = requested.trim();
    if requested.is_empty() {
        return None;
    }
    let candidate = Path::new(requested);
    let candidate = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        project_root.join(candidate)
    };
    let canonical = candidate.canonicalize().ok()?;
    if !canonical.is_file() {
        return None;
    }

    allowed_dirs.iter().find_map(|dir| {
        let dir = Path::new(dir);
        let dir = if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            project_root.join(dir)
        };
        let dir = dir.canonicalize().ok()?;
        let relative = canonical.strip_prefix(&dir).ok()?;
        let hidden = relative
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'));
        let is_config = canonical
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("config.yaml"));
        (!hidden && !is_config).then(|| canonical.clone())
    })
}

/// A path Nerv asked to be uploaded, plus the text with the marker removed.
struct Document {
    path: String,
    remaining_text: String,
}

/// Pull the next `[DOCUMENT: path]` marker out of `text`, if there is one.
fn take_document(text: &str) -> Option<Document> {
    let start = text.find(DOCUMENT_TAG)?;
    let end = start + text[start..].find(']')?;
    let path = text[start + DOCUMENT_TAG.len()..end].trim().to_string();

    let mut remaining = text[..start].to_string();
    remaining.push_str(&text[end + 1..]);

    Some(Document {
        path,
        remaining_text: remaining.trim().to_string(),
    })
}

#[async_trait::async_trait]
impl super::ChannelAdapter for TelegramAdapter {
    async fn start(&self, gateway_tx: mpsc::Sender<IncomingMessage>) -> Result<()> {
        info!("Telegram adapter started");

        let bot = Bot::new(&self.config.bot_token);
        let config = self.config.clone();
        let project_root = self.project_root.clone();

        teloxide::repl(bot, move |bot: Bot, msg: TgMessage| {
            let gateway_tx = gateway_tx.clone();
            let config = config.clone();
            let project_root = project_root.clone();
            async move {
                let text = msg.text().unwrap_or("").to_string();
                if text.is_empty() {
                    return Ok(());
                }

                let chat_id = msg.chat.id;
                // Authorize by the *user* who wrote the message, not the chat:
                // in a group the chat id is shared by everybody in it.
                let Some(user) = msg.from.as_ref() else {
                    return Ok(());
                };
                let sender = user.id.0.to_string();

                // Authorize before the message can reach the brain, which can
                // run shell commands and read the filesystem. Unknown senders
                // get no reply at all — an error message would confirm that a
                // Nerv instance is listening on this bot.
                if !config.is_allowed(&sender) {
                    warn!("Rejected Telegram message from unauthorized sender {sender}");
                    return Ok(());
                }
                if config.private_chats_only && !msg.chat.is_private() {
                    warn!("Ignored Telegram message from a non-private chat");
                    return Ok(());
                }

                let message = Message::new_text(ChannelType::Telegram, &sender, &text);
                let (reply_tx, mut reply_rx) = mpsc::channel::<Message>(1);

                // The 流式推送 typewriter: the brain pushes full-text
                // snapshots while the turn runs; the spool task sends the
                // first snapshot as a message and throttles edits to
                // EDIT_INTERVAL afterwards.
                let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<String>();
                let streamed: Arc<std::sync::Mutex<Option<teloxide::types::MessageId>>> =
                    Arc::new(std::sync::Mutex::new(None));
                let spool_bot = bot.clone();
                let spool_chat = chat_id;
                let spool_streamed = Arc::clone(&streamed);
                let spool = tokio::spawn(async move {
                    let mut last_edit = tokio::time::Instant::now() - EDIT_INTERVAL;
                    let mut last_text = String::new();
                    while let Some(snapshot) = progress_rx.recv().await {
                        if snapshot == last_text {
                            continue;
                        }
                        last_text = snapshot.clone();
                        // Read the id, drop the guard, then await the API
                        // call (a std MutexGuard is not Send across await).
                        let current = *spool_streamed.lock().expect("streamed lock");
                        match current {
                            None => match spool_bot.send_message(spool_chat, snapshot).await {
                                Ok(sent) => {
                                    *spool_streamed.lock().expect("streamed lock") = Some(sent.id);
                                }
                                Err(e) => error!("Failed to send the streaming preview: {e}"),
                            },
                            Some(msg_id) if last_edit.elapsed() >= EDIT_INTERVAL => {
                                last_edit = tokio::time::Instant::now();
                                let _ = spool_bot
                                    .edit_message_text(spool_chat, msg_id, snapshot)
                                    .await;
                            }
                            Some(_) => {}
                        }
                    }
                });

                let incoming = IncomingMessage {
                    message,
                    reply_tx,
                    progress_tx: Some(progress_tx),
                };

                if gateway_tx.send(incoming).await.is_err() {
                    error!("Gateway channel closed");
                    return Ok(());
                }

                // Wait for the brain's response, keeping the typing indicator
                // alive meanwhile (the first tick fires immediately).
                let mut typing = tokio::time::interval(TYPING_REFRESH);
                let reply = loop {
                    tokio::select! {
                        reply = reply_rx.recv() => break reply,
                        _ = typing.tick() => {
                            let _ = bot
                                .send_chat_action(chat_id, teloxide::types::ChatAction::Typing)
                                .await;
                        }
                    }
                };
                // The gateway drops its progress sender when the turn ends,
                // which closes the channel and finishes the spool task.
                let _ = spool.await;

                if let Some(response) = reply {
                    if let Some(response_text) = response.text() {
                        let mut final_text = response_text.to_string();

                        while let Some(document) = take_document(&final_text) {
                            match resolve_document(
                                &document.path,
                                &project_root,
                                &config.document_dirs,
                            ) {
                                Some(path) => {
                                    let input_file = teloxide::types::InputFile::file(&path);
                                    match bot.send_document(chat_id, input_file).await {
                                        Ok(_) => info!("Sent document to Telegram"),
                                        Err(e) => error!("Failed to send document: {}", e),
                                    }
                                }
                                None => warn!(
                                    "Refused document request (missing or outside allowed dirs): {}",
                                    document.path
                                ),
                            }
                            final_text = document.remaining_text;
                        }

                        // A streamed preview exists: edit it to the final
                        // text instead of sending a duplicate message.
                        let streamed_id = streamed.lock().expect("streamed lock").take();
                        if let Some(msg_id) = streamed_id {
                            let _ = bot
                                .edit_message_text(chat_id, msg_id, final_text.clone())
                                .await;
                        }

                        for chunk in chunk_text(&final_text, TELEGRAM_MAX_CHARS) {
                            if chunk.trim().is_empty() {
                                continue;
                            }
                            if streamed_id.is_some() {
                                // Already delivered through the streaming edit.
                                break;
                            }
                            match bot.send_message(chat_id, chunk).await {
                                Ok(_) => info!("Sent reply to Telegram"),
                                Err(e) => error!("Failed to send reply to Telegram: {}", e),
                            }
                        }
                    }
                }

                Ok(())
            }
        })
        .await;

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        info!("Telegram adapter stopped");
        Ok(())
    }

    async fn broadcast(&self, text: &str) {
        let bot = Bot::new(&self.config.bot_token);
        // Private chats share their id with the user, so each allowed user id
        // doubles as the chat id to message.
        for user in &self.config.allowed_users {
            Self::send_chunks(&bot, user, text).await;
        }
    }

    /// Deliver a notification to exactly one sender (E0 定向通知). Senders
    /// outside the allow-list are dropped: the allow-list is the authorization
    /// boundary, and a notification must never widen it.
    async fn send_to(&self, sender: &str, text: &str) {
        if !self
            .config
            .allowed_users
            .iter()
            .any(|u| u.trim() == sender.trim())
        {
            warn!("Notification for {sender} dropped: not in the allow-list");
            return;
        }
        let bot = Bot::new(&self.config.bot_token);
        Self::send_chunks(&bot, sender, text).await;
    }

    fn channel_type(&self) -> ChannelType {
        ChannelType::Telegram
    }
}

impl TelegramAdapter {
    /// Send `text` to one user id in 4096-char chunks.
    async fn send_chunks(bot: &Bot, user: &str, text: &str) {
        let Ok(id) = user.trim().parse::<i64>() else {
            warn!("Invalid Telegram user id: {user}");
            return;
        };
        for chunk in chunk_text(text, TELEGRAM_MAX_CHARS) {
            if let Err(e) = bot.send_message(ChatId(id), chunk).await {
                warn!("Failed to push proactive message to Telegram: {e}");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_document_extracts_the_path_and_strips_the_marker() {
        let doc = take_document("Here you go [DOCUMENT: /tmp/report.pdf] enjoy").unwrap();
        assert_eq!(doc.path, "/tmp/report.pdf");
        assert_eq!(doc.remaining_text, "Here you go  enjoy");
    }

    #[test]
    fn take_document_returns_none_without_a_marker() {
        assert!(take_document("just text").is_none());
    }

    #[test]
    fn take_document_returns_none_for_an_unterminated_marker() {
        // Regression guard: the loop that consumes these must terminate.
        assert!(take_document("oops [DOCUMENT: /tmp/x").is_none());
    }

    #[test]
    fn take_document_consumes_every_marker_in_turn() {
        let mut text = "a [DOCUMENT: /one] b [DOCUMENT: /two] c".to_string();
        let mut paths = Vec::new();
        while let Some(doc) = take_document(&text) {
            paths.push(doc.path);
            text = doc.remaining_text;
        }
        assert_eq!(paths, vec!["/one", "/two"]);
        assert!(!text.contains("DOCUMENT"));
    }

    #[test]
    fn only_allow_listed_senders_are_accepted() {
        let adapter = TelegramAdapter::new(
            TelegramChannelConfig {
                enabled: true,
                bot_token: "token".to_string(),
                allowed_users: vec!["42".to_string()],
                ..Default::default()
            },
            PathBuf::from("."),
        );

        assert!(adapter.config.is_allowed("42"));
        assert!(!adapter.config.is_allowed("43"));
    }

    #[test]
    fn chunk_text_keeps_short_text_whole() {
        assert_eq!(chunk_text("hello", 4096), vec!["hello"]);
        assert!(chunk_text("", 10).is_empty());
    }

    #[test]
    fn chunk_text_respects_the_limit_and_loses_nothing() {
        let text = "word ".repeat(2000);
        let chunks = chunk_text(&text, 100);
        assert!(chunks.iter().all(|c| c.chars().count() <= 100));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn chunk_text_never_splits_a_multibyte_character() {
        let text = "你好吗".repeat(500);
        let chunks = chunk_text(&text, 7);
        assert!(chunks.iter().all(|c| c.chars().count() <= 7));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn chunk_text_prefers_newline_breaks() {
        let text = format!("{}\n{}", "a".repeat(60), "b".repeat(60));
        let chunks = chunk_text(&text, 100);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], format!("{}\n", "a".repeat(60)));
    }

    fn docs_fixture() -> (tempfile::TempDir, Vec<String>) {
        let root = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("personal/workspace/sub")).unwrap();
        std::fs::write(root.path().join("personal/workspace/ok.txt"), "x").unwrap();
        std::fs::write(root.path().join("personal/workspace/sub/deep.txt"), "x").unwrap();
        std::fs::write(root.path().join("personal/workspace/.secret"), "x").unwrap();
        std::fs::write(root.path().join("personal/workspace/config.yaml"), "x").unwrap();
        std::fs::write(root.path().join("config.yaml"), "bot_token: x").unwrap();
        (root, vec!["personal/workspace".to_string()])
    }

    #[test]
    fn documents_resolve_relative_to_the_project_root() {
        let (root, dirs) = docs_fixture();
        let got = resolve_document("personal/workspace/ok.txt", root.path(), &dirs).unwrap();
        assert!(got.ends_with("personal/workspace/ok.txt"));
        assert!(resolve_document("personal/workspace/sub/deep.txt", root.path(), &dirs).is_some());
        let abs = root.path().join("personal/workspace/ok.txt");
        assert!(resolve_document(abs.to_str().unwrap(), root.path(), &dirs).is_some());
    }

    #[test]
    fn documents_outside_allowed_dirs_are_refused() {
        let (root, dirs) = docs_fixture();
        assert!(resolve_document("config.yaml", root.path(), &dirs).is_none());
        assert!(resolve_document("/etc/hosts", root.path(), &dirs).is_none());
        assert!(
            resolve_document("personal/workspace/../../config.yaml", root.path(), &dirs).is_none()
        );
        assert!(resolve_document("", root.path(), &dirs).is_none());
        assert!(resolve_document("personal/workspace/missing", root.path(), &dirs).is_none());
        // The directory itself is not a file.
        assert!(resolve_document("personal/workspace", root.path(), &dirs).is_none());
    }

    #[test]
    fn dotfiles_and_config_yaml_are_refused_even_inside_allowed_dirs() {
        let (root, dirs) = docs_fixture();
        assert!(resolve_document("personal/workspace/.secret", root.path(), &dirs).is_none());
        assert!(resolve_document("personal/workspace/config.yaml", root.path(), &dirs).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape_the_allowed_dirs() {
        let (root, dirs) = docs_fixture();
        std::os::unix::fs::symlink(
            root.path().join("config.yaml"),
            root.path().join("personal/workspace/link.txt"),
        )
        .unwrap();
        assert!(resolve_document("personal/workspace/link.txt", root.path(), &dirs).is_none());
    }
}
