//! Ingestion and search over `kb_chunks` (pgvector HNSW, cosine distance).

use std::sync::Arc;

use common::ids::{DocumentId, KbId, TenantId};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    chunk::chunk_text,
    embed::{to_pgvector, Embedder},
    parse::{extract_text, kind_for},
    KbError,
};

/// Target chunk size in tokens.
pub const CHUNK_TOKENS: usize = 300;
/// Overlap between consecutive chunks in tokens.
pub const CHUNK_OVERLAP: usize = 50;
const EMBED_BATCH: usize = 8;

/// One search result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    /// Chunk text.
    pub content: String,
    /// Cosine similarity (1 = identical).
    pub score: f64,
    /// Source document.
    pub document_id: Uuid,
}

/// Knowledge-base operations.
#[derive(Debug, Clone)]
pub struct KbService {
    db: PgPool,
    embedder: Option<Arc<Embedder>>,
}

impl KbService {
    /// Creates the service. Without an embedder, ingestion and search fail
    /// with a clear error (the server still runs).
    pub fn new(db: PgPool, embedder: Option<Arc<Embedder>>) -> Self {
        Self { db, embedder }
    }

    /// `true` when the embedding model is loaded.
    pub fn available(&self) -> bool {
        self.embedder.is_some()
    }

    fn embedder(&self) -> Result<&Arc<Embedder>, KbError> {
        self.embedder.as_ref().ok_or_else(|| {
            KbError::Embed("embedding model not loaded; run scripts/download_models.sh".into())
        })
    }

    /// Parses, chunks, embeds and stores a document, updating its status.
    pub async fn ingest(
        &self,
        tenant: TenantId,
        kb: KbId,
        doc: DocumentId,
        filename: &str,
        bytes: Vec<u8>,
    ) -> Result<usize, KbError> {
        sqlx::query(
            "UPDATE kb_documents SET status = 'processing', updated_at = now() WHERE id = $1",
        )
        .bind(doc.0)
        .execute(&self.db)
        .await?;
        let result = self.ingest_inner(tenant, kb, doc, filename, bytes).await;
        match &result {
            Ok(n) => {
                tracing::info!(%doc, chunks = n, "document ready");
                sqlx::query(
                    "UPDATE kb_documents SET status = 'ready', error = NULL, updated_at = now() WHERE id = $1",
                )
                .bind(doc.0)
                .execute(&self.db)
                .await?;
            }
            Err(e) => {
                tracing::warn!(%doc, error = %e, "document failed");
                sqlx::query(
                    "UPDATE kb_documents SET status = 'failed', error = $2, updated_at = now() WHERE id = $1",
                )
                .bind(doc.0)
                .bind(e.to_string())
                .execute(&self.db)
                .await?;
            }
        }
        result
    }

    async fn ingest_inner(
        &self,
        tenant: TenantId,
        kb: KbId,
        doc: DocumentId,
        filename: &str,
        bytes: Vec<u8>,
    ) -> Result<usize, KbError> {
        let embedder = self.embedder()?.clone();
        let kind = kind_for(filename)?;
        let counter = embedder.clone();
        let chunks = tokio::task::spawn_blocking(move || {
            let text = extract_text(kind, &bytes)?;
            Ok::<_, KbError>(chunk_text(&text, CHUNK_TOKENS, CHUNK_OVERLAP, |s| {
                counter.count_tokens(s)
            }))
        })
        .await
        .map_err(|e| KbError::Parse(e.to_string()))??;
        if chunks.is_empty() {
            return Err(KbError::Empty);
        }

        // Replace any previous chunks of this document.
        sqlx::query("DELETE FROM kb_chunks WHERE document_id = $1")
            .bind(doc.0)
            .execute(&self.db)
            .await?;
        let mut seq = 0i32;
        for batch in chunks.chunks(EMBED_BATCH) {
            let texts: Vec<String> = batch.iter().map(|c| c.content.clone()).collect();
            let vectors = embedder.embed(texts).await?;
            let mut tx = self.db.begin().await?;
            for (chunk, vector) in batch.iter().zip(vectors) {
                sqlx::query(
                    "INSERT INTO kb_chunks (id, tenant_id, kb_id, document_id, seq, content, token_count, embedding) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8::vector)",
                )
                .bind(Uuid::now_v7())
                .bind(tenant.0)
                .bind(kb.0)
                .bind(doc.0)
                .bind(seq)
                .bind(&chunk.content)
                .bind(chunk.token_count as i32)
                .bind(to_pgvector(&vector))
                .execute(&mut *tx)
                .await?;
                seq += 1;
            }
            tx.commit().await?;
        }
        Ok(chunks.len())
    }

    /// Top `top_k` chunks of `kb` for `query`.
    pub async fn search(
        &self,
        kb: KbId,
        query: &str,
        top_k: i64,
    ) -> Result<Vec<SearchHit>, KbError> {
        let embedder = self.embedder()?;
        let vector = embedder
            .embed(vec![query.to_owned()])
            .await?
            .pop()
            .ok_or_else(|| KbError::Embed("no embedding returned".into()))?;
        let mut tx = self.db.begin().await?;
        // With a per-KB filter, iterative scan keeps HNSW from returning too few rows.
        sqlx::query("SET LOCAL hnsw.iterative_scan = relaxed_order")
            .execute(&mut *tx)
            .await?;
        let rows: Vec<(String, f64, Uuid)> = sqlx::query_as(
            "SELECT content, 1 - (embedding <=> $1::vector) AS score, document_id \
             FROM kb_chunks WHERE kb_id = $2 ORDER BY embedding <=> $1::vector LIMIT $3",
        )
        .bind(to_pgvector(&vector))
        .bind(kb.0)
        .bind(top_k.clamp(1, 20))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows
            .into_iter()
            .map(|(content, score, document_id)| SearchHit {
                content,
                score,
                document_id,
            })
            .collect())
    }
}
