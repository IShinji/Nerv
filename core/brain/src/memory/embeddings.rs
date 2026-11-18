//! Local embeddings for the vector-retrieval path (PRD v2 E10).
//!
//! The embedder wraps `fastembed` with the multilingual-e5-small model (CPU
//! only). The `Embedder` trait is the replaceable component: a third-party
//! API backend or a different local model plugs in behind the same trait.

use std::path::Path;
use std::sync::Mutex;

/// The default embedding model (PRD v2 D6).
pub const DEFAULT_MODEL: &str = "intfloat/multilingual-e5-small";
/// The vector dimension for the default model.
pub const DEFAULT_DIM: u32 = 384;

/// Errors from the embedding pipeline.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("embedding model not available: {0}")]
    Unavailable(String),
    #[error("embedding inference failed: {0}")]
    Inference(String),
}

/// The embedding trait — one method, one input, one output.
///
/// Implementations must be thread-safe (used from the background indexing
/// task and the search path concurrently).
pub trait Embedder: Send + Sync {
    /// Embed a batch of texts. Returns one vector per input.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;
    /// The model name (stored alongside each vector for compatibility checks).
    fn model_name(&self) -> &str;
    /// The vector dimension.
    fn dim(&self) -> u32;
}

/// The default local embedder using fastembed's multilingual-e5-small model.
pub struct FastembedEmbedder {
    inner: Mutex<fastembed::TextEmbedding>,
    model_name: String,
    dim: u32,
}

impl FastembedEmbedder {
    /// Create an embedder with the default model. The model file (~470MB) is
    /// downloaded on first use and cached locally.
    pub fn new(cache_dir: Option<&Path>) -> Result<Self, EmbedError> {
        let model = fastembed::EmbeddingModel::MultilingualE5Small;
        let mut options = fastembed::InitOptions::new(model).with_show_download_progress(true);
        if let Some(dir) = cache_dir {
            options = options.with_cache_dir(dir.to_path_buf());
        }
        let inner = fastembed::TextEmbedding::try_new(options)
            .map_err(|e| EmbedError::Unavailable(e.to_string()))?;
        Ok(Self {
            inner: Mutex::new(inner),
            model_name: DEFAULT_MODEL.to_string(),
            dim: DEFAULT_DIM,
        })
    }
}

impl Embedder for FastembedEmbedder {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        // fastembed requires documents as &[String].
        let documents: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
        let vectors = self
            .inner
            .lock()
            .map_err(|e| EmbedError::Inference(e.to_string()))?
            .embed(documents, None)
            .map_err(|e| EmbedError::Inference(e.to_string()))?;
        Ok(vectors)
    }

    fn model_name(&self) -> &str {
        &self.model_name
    }

    fn dim(&self) -> u32 {
        self.dim
    }
}

/// Long-message chunking: split into 400-char segments and take the maximum
/// similarity per segment pair. Port of the PRD's chunking rule.
pub fn chunk_text(text: &str, chunk_size: usize) -> Vec<String> {
    if text.chars().count() <= chunk_size {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.chars().count() {
        let end = (start + chunk_size).min(text.chars().count());
        // Try to break at a whitespace boundary near the end.
        let slice: String = text.chars().skip(start).take(end - start).collect();
        let boundary = slice
            .rfind(|c: char| c.is_whitespace())
            .map(|pos| pos + 1)
            .unwrap_or(slice.chars().count());
        chunks.push(slice[..boundary].trim().to_string());
        start += boundary;
    }
    chunks.retain(|c| !c.trim().is_empty());
    chunks
}

/// Cosine similarity between two f32 vectors (returns 0.0 on dimension mismatch).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Convert f32 vector to f16 bytes (half the storage, top-10 order 80/82
/// matches per the prototype).
pub fn f32_to_f16_bytes(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 2);
    for value in vector {
        let half = half::f16::from_f32(*value);
        bytes.extend_from_slice(&half.to_le_bytes());
    }
    bytes
}

/// Convert f16 bytes back to f32 vector.
pub fn f16_bytes_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks(2)
        .map(|chunk| half::f16::from_le_bytes([chunk[0], chunk[1]]).to_f32())
        .collect()
}
