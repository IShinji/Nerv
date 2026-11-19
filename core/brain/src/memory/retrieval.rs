//! Hybrid retrieval glue (PRD v2 E10): the vector path over chat history.
//!
//! `HybridRetriever` owns the embedder and the scope's vector store. It
//! indexes appended messages (chunked for long texts) and answers vector
//! queries; the RRF fusion with the keyword path lives in
//! [`super::manager::MemoryManager::search_messages`], which owns the day
//! files needed to turn a vector hit back into a message.
//!
//! Message identity: `{date}#{timestamp}#{chunk}` — the same key the fusion
//! step derives from a [`StoredChatMessage`], so hits map back without a
//! separate index table.

use std::path::Path;
use std::sync::Arc;

use tracing::error;

use super::embeddings::{chunk_text, Embedder, DEFAULT_CHUNK_CHARS};
use super::models::StoredChatMessage;
use super::vector_store::VectorStore;

/// The identity key of an indexed message chunk.
pub fn doc_id(date: &str, msg: &StoredChatMessage, chunk: usize) -> String {
    format!("{}#{}#{}", date, msg.timestamp, chunk)
}

/// Split a doc id back into `(date, timestamp, chunk)`.
pub fn parse_doc_id(id: &str) -> Option<(String, String, usize)> {
    let mut parts = id.split('#');
    let date = parts.next()?.to_string();
    let ts = parts.next()?.to_string();
    let chunk = parts.next()?.parse().ok()?;
    Some((date, ts, chunk))
}

/// The embedder + vector store pair for one scope.
pub struct HybridRetriever {
    embedder: Arc<dyn Embedder>,
    store: VectorStore,
}

impl HybridRetriever {
    pub fn new(project_root: &Path, scope: &str, embedder: Arc<dyn Embedder>) -> Self {
        Self {
            store: VectorStore::new(project_root, scope, embedder.model_name(), embedder.dim()),
            embedder,
        }
    }

    /// Index one message: chunk long texts, embed each chunk, store it.
    /// Failures are logged and swallowed — indexing must never break the
    /// chat flow (the keyword path still works without vectors).
    pub fn index_message(&self, date: &str, msg: &StoredChatMessage) {
        let chunks = chunk_text(&msg.content, DEFAULT_CHUNK_CHARS);
        let refs: Vec<&str> = chunks.iter().map(String::as_str).collect();
        match self.embedder.embed(&refs) {
            Ok(vectors) => {
                for (chunk, vector) in vectors.iter().enumerate() {
                    let bytes = super::embeddings::f32_to_f16_bytes(vector);
                    if let Err(e) = self.store.upsert(
                        &doc_id(date, msg, chunk),
                        &format!("conv:{date}"),
                        &bytes,
                    ) {
                        error!("Vector index upsert failed for {date}: {e}");
                    }
                }
            }
            Err(e) => error!("Embedding failed for {date} message: {e}"),
        }
    }

    /// Vector search: returns raw `(doc id, score)` hits, best first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<(String, f32)> {
        let embedded = match self.embedder.embed(&[query]) {
            Ok(v) => v,
            Err(e) => {
                error!("Query embedding failed: {e}");
                return Vec::new();
            }
        };
        let Some(query_vector) = embedded.into_iter().next() else {
            return Vec::new();
        };
        match self.store.search(&query_vector, limit) {
            Ok(hits) => hits.into_iter().map(|h| (h.id, h.score)).collect(),
            Err(e) => {
                error!("Vector search failed: {e}");
                Vec::new()
            }
        }
    }

    /// How many chunks are indexed (used to decide on a backfill).
    pub fn count(&self) -> usize {
        self.store.count()
    }
}

/// Reciprocal Rank Fusion over the keyword and vector rankings. Each hit's
/// fused score is `Σ 1/(k + rank)` across the lists it appears in; `k = 60`
/// is the standard constant from the RRF paper.
pub fn rrf_fuse(keyword_ids: &[String], vector_ids: &[String], k: f64) -> Vec<(String, f64)> {
    let mut scores: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for (rank, id) in keyword_ids.iter().enumerate() {
        *scores.entry(id.clone()).or_default() += 1.0 / (k + rank as f64 + 1.0);
    }
    for (rank, id) in vector_ids.iter().enumerate() {
        *scores.entry(id.clone()).or_default() += 1.0 / (k + rank as f64 + 1.0);
    }
    let mut fused: Vec<(String, f64)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1));
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_prefers_hits_in_both_lists() {
        let kw: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let vec: Vec<String> = vec!["b".into(), "d".into(), "e".into()];
        let fused = rrf_fuse(&kw, &vec, 60.0);
        // "b" appears in both lists, so it outranks the list leaders.
        assert_eq!(fused[0].0, "b");
        assert!(fused[1].1 >= fused[2].1);
        assert_eq!(fused.len(), 5);
    }

    #[test]
    fn doc_ids_round_trip() {
        let msg = StoredChatMessage::new("user", "hello");
        let id = doc_id("2026-11-19", &msg, 2);
        let (date, ts, chunk) = parse_doc_id(&id).unwrap();
        assert_eq!(date, "2026-11-19");
        assert_eq!(ts, msg.timestamp.to_string());
        assert_eq!(chunk, 2);
    }
}
