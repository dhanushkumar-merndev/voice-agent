//! Knowledge base: document parsing (txt, md, pdf), token-aware chunking,
//! local bge-m3 embeddings via ONNX Runtime (no network hop) and pgvector
//! HNSW cosine search.

pub mod chunk;
pub mod embed;
pub mod parse;
pub mod store;

pub use embed::Embedder;
pub use store::{KbService, SearchHit};

/// Knowledge-base error.
#[derive(Debug, thiserror::Error)]
pub enum KbError {
    /// The file type is not supported.
    #[error("unsupported file type: {0}")]
    Unsupported(String),
    /// The document could not be parsed.
    #[error("could not read document: {0}")]
    Parse(String),
    /// The document contains no text.
    #[error("document has no extractable text")]
    Empty,
    /// Embedding model missing or failed.
    #[error("embedding failed: {0}")]
    Embed(String),
    /// Database failure.
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}
