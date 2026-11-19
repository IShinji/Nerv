//! Manager for long-term and short-term memory persistence.
//!
//! Port of `brain/nerv/memory/manager.py`. Conversations are stored per
//! *scope* so two people talking to the same Nerv instance never read each
//! other's history:
//!
//! ```text
//! personal/memory/conversations/YYYY-MM-DD.jsonl          (default scope)
//! personal/memory/scopes/<scope>/conversations/...jsonl   (named scope)
//! ```
//!
//! Each conversation file is JSON Lines — one message per line — so appending
//! a turn is O(1). Legacy `YYYY-MM-DD.json` files (a single
//! `DailyConversation` object) are still read for backwards compatibility.
//!
//! Long-term `facts.md` is deliberately *not* scoped: it holds what Nerv knows
//! about its owner, and that follows the owner across channels.
//!
//! Writes take an exclusive cross-process file lock, because the MCP server
//! runs in a separate process from the brain and both can append — the
//! python memory tools use the same lock protocol on the same lock files.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use chrono::Local;
use regex::Regex;
use tracing::{error, info};

use super::embeddings::Embedder;
use super::models::{DailyConversation, StoredChatMessage};
use super::retrieval::{parse_doc_id, rrf_fuse, HybridRetriever};

pub const DEFAULT_SCOPE: &str = "default";

/// One conversation message matched by [`MemoryManager::search_messages`].
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub date: String,
    pub message: StoredChatMessage,
    pub score: f64,
}

/// Manages persistence of conversation and facts for one scope.
pub struct MemoryManager {
    pub scope: String,
    pub conv_dir: PathBuf,
    pub facts_file: PathBuf,
    lock_path: PathBuf,
    facts_lock_path: PathBuf,
    /// Vector retrieval (PRD v2 E10). `None` until first use — the embedder
    /// model (~470MB) must never block chat flow on first run.
    retriever: OnceLock<Option<Arc<HybridRetriever>>>,
    embedder: Option<Arc<dyn Embedder>>,
}

impl MemoryManager {
    pub fn new(project_root: &Path, scope: &str) -> Self {
        let scope = if scope.is_empty() {
            DEFAULT_SCOPE.to_string()
        } else {
            scope.to_string()
        };
        let memory_dir = project_root.join("personal").join("memory");
        let conv_dir = if scope == DEFAULT_SCOPE {
            memory_dir.join("conversations")
        } else {
            memory_dir.join("scopes").join(&scope).join("conversations")
        };
        // Facts are shared across scopes on purpose — see the module docs.
        let facts_file = memory_dir.join("facts.md");
        let lock_path = conv_dir.join(".lock");
        let facts_lock_path = memory_dir.join(".facts.lock");
        let manager = Self {
            scope,
            conv_dir,
            facts_file,
            lock_path,
            facts_lock_path,
            retriever: OnceLock::new(),
            embedder: None,
        };
        manager.ensure_directories();
        manager
    }

    /// Enable the vector retrieval path with an explicit embedder (tests
    /// inject a fake here; production uses the fastembed default).
    pub fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// The scope's retriever. Only scopes constructed with an embedder get
    /// the vector path — an embedder that fails to initialize stays `None`
    /// and the keyword path is the permanent fallback.
    fn retriever(&self) -> Option<&Arc<HybridRetriever>> {
        self.retriever
            .get_or_init(|| {
                self.embedder.as_ref().map(|embedder| {
                    Arc::new(HybridRetriever::new(
                        &self.project_root(),
                        &self.scope,
                        Arc::clone(embedder),
                    ))
                })
            })
            .as_ref()
    }

    /// The project root recovered from the memory directory layout
    /// (`<root>/personal/...`); used to place the vector store.
    fn project_root(&self) -> PathBuf {
        let memory_dir = self
            .facts_file
            .parent()
            .unwrap_or_else(|| Path::new("personal/memory"));
        memory_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Build a manager with the vector path enabled when the config says so.
    /// The embedder is lazy — the ~470MB model loads on first use, never at
    /// construction.
    pub fn manager_for(project_root: &Path, scope: &str, vector_retrieval: bool) -> Self {
        let manager = Self::new(project_root, scope);
        if vector_retrieval {
            manager.with_embedder(Arc::new(super::embeddings::LazyEmbedder::new()))
        } else {
            manager
        }
    }

    fn ensure_directories(&self) {
        let _ = std::fs::create_dir_all(&self.conv_dir);
        if !self.facts_file.exists() {
            if let Some(parent) = self.facts_file.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::File::create(&self.facts_file);
        }
    }

    // ── Paths ────────────────────────────────────────────────────────────────

    pub fn today_str() -> String {
        Local::now().format("%Y-%m-%d").to_string()
    }

    pub fn daily_file(&self, date_str: &str) -> PathBuf {
        self.conv_dir.join(format!("{date_str}.jsonl"))
    }

    fn legacy_daily_file(&self, date_str: &str) -> PathBuf {
        self.conv_dir.join(format!("{date_str}.json"))
    }

    fn summary_file(&self, date_str: &str) -> PathBuf {
        self.conv_dir.join(format!("{date_str}.summary.md"))
    }

    /// `(date_str, path)` for every stored day, oldest first.
    fn dated_files(&self) -> Vec<(String, PathBuf)> {
        let mut found: Vec<(String, PathBuf)> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.conv_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                match path.extension().and_then(|e| e.to_str()) {
                    Some("jsonl") | Some("json") => {
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            let date = stem.to_string();
                            // JSONL wins over a same-day legacy file.
                            match found.iter_mut().find(|(d, _)| *d == date) {
                                Some(slot) if path.extension() == Some("jsonl".as_ref()) => {
                                    slot.1 = path
                                }
                                _ => found.push((date, path)),
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
    }

    // ── Reading ──────────────────────────────────────────────────────────────

    /// Read one day's messages from a JSONL or legacy JSON file.
    fn read_messages(&self, path: &Path) -> Vec<StoredChatMessage> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) => {
                error!("Failed to read conversation {}: {e}", path.display());
                return Vec::new();
            }
        };

        if path.extension() == Some("json".as_ref()) {
            return match serde_json::from_str::<DailyConversation>(&raw) {
                Ok(conv) => conv.messages,
                Err(e) => {
                    error!("Failed to parse conversation {}: {e}", path.display());
                    Vec::new()
                }
            };
        }

        let mut messages = Vec::new();
        for (line_no, line) in raw.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<StoredChatMessage>(line) {
                Ok(msg) => messages.push(msg),
                // One corrupt line must not lose the rest of the day.
                Err(e) => error!(
                    "Skipping bad message at {}:{}: {e}",
                    path.display(),
                    line_no + 1
                ),
            }
        }
        messages
    }

    /// Load one day's conversation from disk, or return an empty one.
    pub fn load_conversation(&self, date_str: &str) -> DailyConversation {
        let mut path = self.daily_file(date_str);
        if !path.exists() {
            let legacy = self.legacy_daily_file(date_str);
            if legacy.exists() {
                path = legacy;
            }
        }
        let messages = if path.exists() {
            self.read_messages(&path)
        } else {
            Vec::new()
        };

        let summary_path = self.summary_file(date_str);
        let summary = std::fs::read_to_string(&summary_path).unwrap_or_default();

        DailyConversation {
            date: date_str.to_string(),
            messages,
            summary,
        }
    }

    /// Load the most recent messages across days up to the limit.
    pub fn load_recent_messages(&self, limit: usize) -> Vec<StoredChatMessage> {
        let mut messages: Vec<StoredChatMessage> = Vec::new();
        // Walk backwards from the newest day until we have enough.
        for (_date_str, path) in self.dated_files().iter().rev() {
            for msg in self.read_messages(path).into_iter().rev() {
                messages.push(msg);
                if messages.len() >= limit {
                    break;
                }
            }
            if messages.len() >= limit {
                break;
            }
        }
        // Reverse back so the oldest is first, newest is last.
        messages.reverse();
        messages
    }

    /// Find past messages in *this scope* matching `query`.
    ///
    /// Retrieval here is keyword-based on purpose. The corpus is one owner's
    /// chat history — small enough that scanning it costs less than keeping an
    /// embedding index correct, and the queries that matter are dominated by
    /// rare literal terms anyway. Scoring favours, in order: matching more
    /// distinct query terms, matching the whole phrase, and recency as the
    /// tie-break. `since`/`until` are inclusive `YYYY-MM-DD` bounds; `role`
    /// filters to one speaker.
    pub fn search_messages(
        &self,
        query: &str,
        limit: usize,
        since: &str,
        until: &str,
        role: &str,
    ) -> Vec<SearchHit> {
        let phrase = query.trim().to_lowercase();
        if phrase.is_empty() {
            return Vec::new();
        }
        let terms: Vec<String> = Regex::new(r"\W+")
            .expect("static regex")
            .split(&query.to_lowercase())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();

        let mut hits: Vec<SearchHit> = Vec::new();
        for (date_str, path) in self.dated_files() {
            if !since.is_empty() && date_str.as_str() < since {
                continue;
            }
            if !until.is_empty() && date_str.as_str() > until {
                continue;
            }
            for msg in self.read_messages(&path) {
                if !role.is_empty() && msg.role != role {
                    continue;
                }
                let haystack = msg.content.to_lowercase();
                let matched = terms
                    .iter()
                    .filter(|t| haystack.contains(t.as_str()))
                    .count();
                if matched == 0 && !haystack.contains(&phrase) {
                    continue;
                }
                let mut score = matched as f64;
                if haystack.contains(&phrase) {
                    // A whole-phrase hit outranks scattered term hits.
                    score += terms.len() as f64 + 1.0;
                }
                hits.push(SearchHit {
                    date: date_str.clone(),
                    message: msg,
                    score,
                });
            }
        }

        // Newest first within equal scores, so recency breaks ties.
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(b.date.cmp(&a.date)));
        hits.truncate(limit);
        hits
    }

    /// Hybrid search (PRD v2 E10): the keyword ranking above fused with the
    /// vector ranking through Reciprocal Rank Fusion. Without an embedder
    /// this is exactly the keyword path.
    pub fn search_messages_hybrid(
        &self,
        query: &str,
        limit: usize,
        since: &str,
        until: &str,
        role: &str,
    ) -> Vec<SearchHit> {
        let keyword = self.search_messages(query, limit * 2, since, until, role);
        let Some(retriever) = self.retriever() else {
            return keyword;
        };

        // Backfill the vector index once, on the first hybrid search of an
        // empty store — afterwards appends keep it current.
        if retriever.count() == 0 {
            self.backfill_index(retriever);
        }

        let vector = retriever.search(query, limit * 2);
        // A hit's identity key must match the indexed doc id (date#timestamp).
        let mut keyword_map: std::collections::HashMap<String, SearchHit> =
            std::collections::HashMap::new();
        let keyword_ids: Vec<String> = keyword
            .iter()
            .map(|h| {
                let id = format!("{}#{}", h.date, h.message.timestamp);
                keyword_map.insert(id.clone(), h.clone());
                id
            })
            .collect();
        let vector_ids: Vec<String> = vector.iter().map(|(id, _)| id.clone()).collect();
        let fused = rrf_fuse(&keyword_ids, &vector_ids, 60.0);

        let mut out: Vec<SearchHit> = Vec::new();
        for (id, score) in fused.into_iter().take(limit) {
            let hit = match keyword_map.remove(&id) {
                Some(mut hit) => {
                    hit.score = score;
                    hit
                }
                None => match self.message_by_doc_id(&id) {
                    Some(mut hit) => {
                        hit.score = score;
                        hit
                    }
                    None => continue,
                },
            };
            out.push(hit);
        }
        out
    }

    /// Look a doc id up in its day file: `(date, timestamp)` identifies the
    /// message, the trailing `#chunk` selects one chunk of many.
    fn message_by_doc_id(&self, id: &str) -> Option<SearchHit> {
        let (date, ts, _chunk) = parse_doc_id(id)?;
        let path = self.daily_file(&date);
        self.read_messages(&path)
            .into_iter()
            .find(|m| m.timestamp.to_string() == ts)
            .map(|message| SearchHit {
                date,
                message,
                score: 0.0,
            })
    }

    /// Index the whole conversation history into the vector store. Runs on
    /// the caller's thread — invoked once from the first hybrid search.
    fn backfill_index(&self, retriever: &Arc<HybridRetriever>) {
        info!("Backfilling the vector index for scope {}", self.scope);
        for (date_str, path) in self.dated_files() {
            for msg in self.read_messages(&path) {
                retriever.index_message(&date_str, &msg);
            }
        }
    }

    // ── Writing ──────────────────────────────────────────────────────────────

    /// Replace a whole day's conversation on disk.
    pub fn save_conversation(&self, conv: &DailyConversation) {
        let path = self.daily_file(&conv.date);
        let body = conv
            .messages
            .iter()
            .map(|msg| serde_json::to_string(msg).unwrap_or_default() + "\n")
            .collect::<String>();
        let result = self.locked(&self.lock_path, || {
            self.atomic_write(&path, &body)?;
            if !conv.summary.is_empty() {
                self.atomic_write(&self.summary_file(&conv.date), &conv.summary)?;
            }
            // The JSONL file is now authoritative for this day.
            let _ = std::fs::remove_file(self.legacy_daily_file(&conv.date));
            Ok(())
        });
        if let Err(e) = result {
            error!("Failed to save conversation to {}: {e}", path.display());
        }
    }

    /// Append a single message to today's conversation.
    ///
    /// O(1) append under the scope lock — never rewrites the day.
    pub fn append_message(&self, role: &str, content: &str) {
        let today = Self::today_str();
        let msg = StoredChatMessage::new(role, content);
        let path = self.daily_file(&today);
        let result = self.locked(&self.lock_path, || {
            self.migrate_legacy_day(&today)?;
            let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
            writeln!(file, "{}", serde_json::to_string(&msg).unwrap_or_default())
        });
        if let Err(e) = result {
            error!("Failed to append message to {}: {e}", path.display());
            return;
        }
        // Index for vector retrieval off the write path — indexing must
        // never block or fail the append.
        if let Some(retriever) = self.retriever() {
            let retriever = Arc::clone(retriever);
            let today = today.clone();
            std::thread::spawn(move || retriever.index_message(&today, &msg));
        }
    }

    /// Fold a pre-JSONL day file into the JSONL file. Caller holds the lock.
    fn migrate_legacy_day(&self, date_str: &str) -> std::io::Result<()> {
        let legacy = self.legacy_daily_file(date_str);
        if !legacy.exists() {
            return Ok(());
        }
        let path = self.daily_file(date_str);
        let existing = if path.exists() {
            self.read_messages(&path)
        } else {
            Vec::new()
        };
        let mut merged = self.read_messages(&legacy);
        merged.extend(existing);
        let body = merged
            .iter()
            .map(|msg| serde_json::to_string(msg).unwrap_or_default() + "\n")
            .collect::<String>();
        self.atomic_write(&path, &body)?;
        let _ = std::fs::remove_file(&legacy);
        info!("Migrated legacy conversation {} to JSONL", legacy.display());
        Ok(())
    }

    /// Write `body` to `path` via a temp file + rename. Caller holds the lock.
    fn atomic_write(&self, path: &Path, body: &str) -> std::io::Result<()> {
        let tmp = path.with_extension(format!(
            "{}-tmp-{}",
            path.extension().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, path)
    }

    // ── Summaries ────────────────────────────────────────────────────────────

    /// Return the stored rolling summary for a day, or an empty string.
    pub fn read_summary(&self, date_str: &str) -> String {
        let path = self.summary_file(date_str);
        if !path.exists() {
            return String::new();
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => text.trim().to_string(),
            Err(e) => {
                error!("Failed to read summary {}: {e}", path.display());
                String::new()
            }
        }
    }

    /// Store the rolling summary for a day.
    pub fn write_summary(&self, date_str: &str, summary: &str) {
        let result = self.locked(&self.lock_path, || {
            self.atomic_write(
                &self.summary_file(date_str),
                &format!("{}\n", summary.trim()),
            )
        });
        if let Err(e) = result {
            error!("Failed to write summary for {date_str}: {e}");
        }
    }

    /// `(date, summary)` for the most recent summarised days before
    /// `date_str`, newest first.
    pub fn summaries_before(&self, date_str: &str, limit: usize) -> Vec<(String, String)> {
        let mut found: Vec<(String, String)> = Vec::new();
        for (day, _path) in self.dated_files().iter().rev() {
            if day.as_str() >= date_str {
                continue;
            }
            let summary = self.read_summary(day);
            if !summary.is_empty() {
                found.push((day.clone(), summary));
            }
            if found.len() >= limit {
                break;
            }
        }
        found
    }

    /// Past days that hold messages but no summary yet, newest first.
    ///
    /// Today is excluded: it is still being written to, so summarising it now
    /// would only be redone on the next turn.
    pub fn days_needing_summary(&self, limit: usize) -> Vec<String> {
        let today = Self::today_str();
        let mut pending: Vec<String> = Vec::new();
        for (day, path) in self.dated_files().iter().rev() {
            if day.as_str() >= today.as_str() || !self.read_summary(day).is_empty() {
                continue;
            }
            if !self.read_messages(path).is_empty() {
                pending.push(day.clone());
            }
            if pending.len() >= limit {
                break;
            }
        }
        pending
    }

    // ── Facts ────────────────────────────────────────────────────────────────

    /// Read long-term facts.
    pub fn read_facts(&self) -> String {
        std::fs::read_to_string(&self.facts_file).unwrap_or_default()
    }

    /// Split `facts.md` into `(heading, body)` pairs.
    ///
    /// Sections are Markdown ATX headings (`#`..`######`). Anything before the
    /// first heading is returned under the empty heading, so an unstructured
    /// legacy facts file still round-trips.
    pub fn fact_sections(&self) -> Vec<(String, String)> {
        let text = self.read_facts();
        if text.trim().is_empty() {
            return Vec::new();
        }

        static HEADING_RE: std::sync::LazyLock<Regex> =
            std::sync::LazyLock::new(|| Regex::new(r"^#{1,6}\s+\S").expect("static regex"));

        let mut sections: Vec<(String, Vec<String>)> = Vec::new();
        let mut preamble: Vec<String> = Vec::new();
        for line in text.lines() {
            if HEADING_RE.is_match(line) {
                let heading = line.trim_start_matches('#').trim().to_string();
                sections.push((heading, Vec::new()));
            } else if let Some(last) = sections.last_mut() {
                last.1.push(line.to_string());
            } else {
                preamble.push(line.to_string());
            }
        }

        let mut result: Vec<(String, String)> = Vec::new();
        if preamble.iter().any(|line| !line.trim().is_empty()) {
            result.push((String::new(), preamble.join("\n").trim().to_string()));
        }
        result.extend(
            sections
                .into_iter()
                .map(|(head, body)| (head, body.join("\n").trim().to_string())),
        );
        result
    }

    /// A one-line-per-section table of contents for `facts.md`.
    pub fn facts_outline(&self) -> String {
        let sections = self.fact_sections();
        if sections.is_empty() {
            return String::new();
        }
        let mut lines: Vec<String> = Vec::new();
        for (heading, body) in sections {
            let label = if heading.is_empty() {
                "(untitled)"
            } else {
                &heading
            };
            let first = body
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .to_string();
            let first = if first.chars().count() > 80 {
                format!("{}…", first.chars().take(80).collect::<String>())
            } else {
                first
            };
            if first.is_empty() {
                lines.push(format!("- {label}"));
            } else {
                lines.push(format!("- {label}: {first}"));
            }
        }
        lines.join("\n")
    }

    /// Return one section's body, matched case-insensitively by heading.
    pub fn read_fact_section(&self, heading: &str) -> String {
        let wanted = heading.trim().to_lowercase();
        for (head, body) in self.fact_sections() {
            if head.to_lowercase() == wanted {
                return body;
            }
        }
        String::new()
    }

    /// Append a new long-term fact.
    pub fn append_fact(&self, fact: &str) {
        let _ = self.locked(&self.facts_lock_path, || {
            let mut content = self.read_facts();
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(&format!("- {fact}\n"));
            self.atomic_write(&self.facts_file, &content)
        });
    }

    // ── Locking ──────────────────────────────────────────────────────────────

    /// Hold an exclusive cross-process lock on `lock_path` for the closure.
    fn locked<T>(
        &self,
        lock_path: &Path,
        body: impl FnOnce() -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path)?;
        #[cfg(unix)]
        unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX);
        }
        let result = body();
        #[cfg(unix)]
        unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN);
        }
        result
    }
}
