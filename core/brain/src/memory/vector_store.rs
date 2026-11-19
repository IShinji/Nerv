//! SQLite vector store for the retrieval path (PRD v2 E10).
//!
//! Stores f16-quantized vectors per scope per model. The store is per-scope
//! (same isolation as the conversation files) and per-model (vectors from
//! different models never mix; a model change requires a rebuild).
//!
//! By default the database is local SQLite; a third-party vector database is
//! a configurable option that only sends vectors and message ids (PRD D5).

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;
use tracing::{error, warn};

use super::embeddings::EmbedError;

/// A vector-search hit.
#[derive(Debug, Clone)]
pub struct VectorHit {
    pub id: String,
    pub source: String,
    pub score: f32,
}

/// The vector store backed by local SQLite. One database per scope.
pub struct VectorStore {
    conn: Mutex<Connection>,
    model_name: String,
    dim: u32,
}

impl VectorStore {
    /// Open (and create tables if needed) the vector store for one scope.
    pub fn new(project_root: &Path, scope: &str, model_name: &str, dim: u32) -> Self {
        let memory_dir = project_root.join("personal").join("memory");
        let dir = if scope == crate::memory::DEFAULT_SCOPE {
            memory_dir.join("vectors")
        } else {
            memory_dir.join("scopes").join(scope).join("vectors")
        };
        let _ = std::fs::create_dir_all(&dir);
        let db_path = dir.join("vectors.db");

        let conn = Connection::open(&db_path).unwrap_or_else(|e| {
            error!("Failed to open vector store {}: {e}", db_path.display());
            Connection::open_in_memory().expect("in-memory fallback")
        });

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vectors (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                model TEXT NOT NULL,
                dim INTEGER NOT NULL,
                vector BLOB NOT NULL,
                created_at REAL
            );
            CREATE INDEX IF NOT EXISTS idx_scope ON vectors(source);",
        )
        .ok();

        Self {
            conn: Mutex::new(conn),
            model_name: model_name.to_string(),
            dim,
        }
    }

    /// Store a vector for a message or fact section.
    pub fn upsert(&self, id: &str, source: &str, vector: &[u8]) -> Result<(), EmbedError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| EmbedError::Inference(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO vectors (id, source, model, dim, vector, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                id,
                source,
                self.model_name,
                self.dim,
                vector,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0)
            ],
        )
        .map_err(|e| EmbedError::Inference(e.to_string()))?;
        Ok(())
    }

    /// Remove all vectors for a source (e.g. a day's conversation file).
    pub fn remove_by_source(&self, source: &str) {
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute("DELETE FROM vectors WHERE source = ?1", [source]);
        }
    }

    /// Cosine-similarity search over the stored f16 vectors (linear scan —
    /// the corpus is one owner's chat history, small enough to scan).
    /// Returns hits with scores, filtered by model and dimension.
    pub fn search(&self, query_vector: &[f32], limit: usize) -> Result<Vec<VectorHit>, EmbedError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| EmbedError::Inference(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT id, source, vector FROM vectors WHERE model = ?1 AND dim = ?2")
            .map_err(|e| EmbedError::Inference(e.to_string()))?;

        let rows = stmt
            .query_map([self.model_name.as_str(), &self.dim.to_string()], |row| {
                let id: String = row.get(0)?;
                let source: String = row.get(1)?;
                let vector_blob: Vec<u8> = row.get(2)?;
                Ok((id, source, vector_blob))
            })
            .map_err(|e| EmbedError::Inference(e.to_string()))?;

        let mut hits: Vec<VectorHit> = Vec::new();
        for row in rows {
            let (id, source, blob) = match row {
                Ok(r) => r,
                Err(e) => {
                    warn!("Vector row read failed: {e}");
                    continue;
                }
            };
            let stored: Vec<f32> = blob
                .chunks(2)
                .map(|c| half::f16::from_le_bytes([c[0], c[1]]).to_f32())
                .collect();
            let score = super::embeddings::cosine_similarity(query_vector, &stored);
            hits.push(VectorHit { id, source, score });
        }

        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(limit);
        Ok(hits)
    }

    /// The number of vectors stored (for tests and diagnostics).
    pub fn count(&self) -> usize {
        self.conn
            .lock()
            .map(|conn| {
                conn.query_row("SELECT COUNT(*) FROM vectors", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap_or(0) as usize
            })
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_search_and_count() {
        let dir = tempfile::tempdir().unwrap();
        let store = VectorStore::new(dir.path(), "test-scope", "test-model", 4);
        assert_eq!(store.count(), 0);

        // Two vectors in opposite directions.
        let a: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
        let b: Vec<f32> = vec![0.0, 1.0, 0.0, 0.0];
        let a_bytes = crate::memory::embeddings::f32_to_f16_bytes(&a);
        let b_bytes = crate::memory::embeddings::f32_to_f16_bytes(&b);
        store.upsert("msg-a", "conv:2026-03-01", &a_bytes).unwrap();
        store.upsert("msg-b", "conv:2026-03-01", &b_bytes).unwrap();
        assert_eq!(store.count(), 2);

        let query = vec![1.0, 0.0, 0.0, 0.0];
        let hits = store.search(&query, 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(
            hits[0].score > hits[1].score,
            "closest match should rank first"
        );
    }

    #[test]
    fn different_models_dont_mix() {
        let dir = tempfile::tempdir().unwrap();
        let store_a = VectorStore::new(dir.path(), "test", "model-a", 2);
        let store_b = VectorStore::new(dir.path(), "test", "model-b", 2);
        store_a
            .upsert(
                "v1",
                "src",
                &crate::memory::embeddings::f32_to_f16_bytes(&[1.0, 0.0]),
            )
            .unwrap();
        // model-b's store is empty — vectors from model-a don't participate
        // in model-b's search (the search filters by model name).
        let query = vec![1.0, 0.0];
        assert!(store_b.search(&query, 10).unwrap().is_empty());
        assert_eq!(store_a.search(&query, 10).unwrap().len(), 1);
    }
}
