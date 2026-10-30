//! Audio cache for the greeting, fixed lines and common reply phrases
//! (`tts_cache`).
//!
//! Lookup order: memory (bounded LRU) → `tts_cache` row + object storage →
//! synthesize once, then store in both. Short reply clauses synthesized
//! during calls are also offered to the cache (`LineAudio::learn`), so a
//! phrase like "Got it." costs TTS only the first time it is said. Keys hash model, speaker, language,
//! pace and text, so any voice change produces new audio. Audio is stored
//! as raw 8 kHz μ-law, ready to stream to Plivo without conversion.

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use common::{
    assets::{Pricing, Voice},
    ids::{TenantId, DEFAULT_TENANT},
    storage::Storage,
    AppError, ErrorCode,
};
use moka::sync::Cache;
use pipeline::LineAudio;
use providers::tts::{self, TtsProvider};
use sqlx::PgPool;

use super::{
    spend::{self, SpendKind},
    util::sha256_hex,
};

/// In-memory cap for cached audio.
const MEMORY_BYTES: u64 = 64 * 1024 * 1024;
const SYNTH_TIMEOUT: Duration = Duration::from_secs(10);

/// The cache.
pub struct LineCache {
    memory: Cache<String, Bytes>,
    db: PgPool,
    storage: Storage,
    tts: Arc<dyn TtsProvider>,
    pricing: Pricing,
}

impl std::fmt::Debug for LineCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LineCache")
            .field("entries", &self.memory.entry_count())
            .finish_non_exhaustive()
    }
}

/// Cache key for `text` spoken in `voice`.
pub fn key(voice: &Voice, text: &str) -> String {
    sha256_hex(
        format!(
            "{}|{}|{}|{:.2}|{}",
            voice.model, voice.speaker, voice.language, voice.pace, text
        )
        .as_bytes(),
    )
}

impl LineCache {
    /// Creates the cache.
    pub fn new(db: PgPool, storage: Storage, tts: Arc<dyn TtsProvider>, pricing: Pricing) -> Self {
        Self {
            memory: Cache::builder()
                .weigher(|_k: &String, v: &Bytes| u32::try_from(v.len()).unwrap_or(u32::MAX))
                .max_capacity(MEMORY_BYTES)
                .build(),
            db,
            storage,
            tts,
            pricing,
        }
    }

    /// Whether the audio is already in memory (everything stored is loaded
    /// into memory by [`Self::preload`] at startup).
    pub fn has(&self, voice: &Voice, text: &str) -> bool {
        self.memory.contains_key(&key(voice, text))
    }

    /// Returns cached audio, synthesizing and storing it on a miss. The TTS
    /// cost of a miss goes to the shared spend ledger.
    pub async fn get_or_synthesize(
        &self,
        tenant: TenantId,
        voice: &Voice,
        text: &str,
    ) -> Result<Bytes, AppError> {
        let (audio, cost) = self.get_or_synthesize_billed(tenant, voice, text).await?;
        if cost > 0.0 {
            spend::record(
                &self.db,
                tenant,
                SpendKind::TtsCache,
                &voice.model,
                text.chars().count() as u64,
                cost,
            )
            .await;
        }
        Ok(audio)
    }

    /// Like [`Self::get_or_synthesize`], but returns the TTS cost (₹, 0 on
    /// a cache hit) for the caller to bill, e.g. to the call it is for.
    pub async fn get_or_synthesize_billed(
        &self,
        tenant: TenantId,
        voice: &Voice,
        text: &str,
    ) -> Result<(Bytes, f64), AppError> {
        let hash = key(voice, text);
        if let Some(audio) = self.memory.get(&hash) {
            return Ok((audio, 0.0));
        }
        let row: Option<(String,)> =
            sqlx::query_as("SELECT storage_key FROM tts_cache WHERE tenant_id = $1 AND hash = $2")
                .bind(tenant.0)
                .bind(&hash)
                .fetch_optional(&self.db)
                .await?;
        if let Some((storage_key,)) = row {
            match self.storage.get(&storage_key).await {
                Ok(data) => {
                    let audio = Bytes::from(data);
                    self.memory.insert(hash, audio.clone());
                    return Ok((audio, 0.0));
                }
                Err(e) => {
                    tracing::warn!(error = %e, %storage_key, "cached audio missing; resynthesizing")
                }
            }
        }

        let audio = tts::synthesize(&*self.tts, voice, text, SYNTH_TIMEOUT)
            .await
            .map_err(|e| AppError::new(ErrorCode::ProviderError, format!("tts failed: {e}")))?;
        // Billed even if storing fails below: the synthesis happened.
        let cost = self
            .pricing
            .tts_cost(&voice.model, text.chars().count() as u64)
            .unwrap_or(0.0);
        persist(&self.db, &self.storage, tenant, &hash, voice, text, &audio).await?;
        self.memory.insert(hash, audio.clone());
        Ok((audio, cost))
    }

    /// Loads cached audio from the database into memory, so phrases learned
    /// on earlier calls are available right after a restart.
    pub async fn preload(&self, tenant: TenantId) {
        let rows: Result<Vec<(String, String)>, _> = sqlx::query_as(
            "SELECT hash, storage_key FROM tts_cache WHERE tenant_id = $1 \
             ORDER BY created_at DESC LIMIT 2000",
        )
        .bind(tenant.0)
        .fetch_all(&self.db)
        .await;
        let rows = match rows {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "could not list cached audio");
                return;
            }
        };
        let mut loaded = 0usize;
        for (hash, storage_key) in rows {
            if self.memory.contains_key(&hash) {
                continue;
            }
            if let Ok(data) = self.storage.get(&storage_key).await {
                self.memory.insert(hash, Bytes::from(data));
                loaded += 1;
            }
        }
        tracing::info!(loaded, "cached phrase audio loaded");
    }

    /// Loads or synthesizes several lines, two at a time so a call's own
    /// speech never queues behind a burst of TTS requests (Sarvam rate
    /// limits bursts); failures are logged.
    pub async fn warm(&self, tenant: TenantId, voice: &Voice, texts: Vec<String>) {
        for pair in texts.chunks(2) {
            let jobs = pair
                .iter()
                .map(|t| self.get_or_synthesize(tenant, voice, t));
            for (text, result) in pair.iter().zip(futures_util::future::join_all(jobs).await) {
                if let Err(e) = result {
                    tracing::warn!(error = %e, %text, "could not warm fixed line");
                }
            }
        }
    }
}

/// Stores audio in object storage and records it in `tts_cache`.
async fn persist(
    db: &PgPool,
    storage: &Storage,
    tenant: TenantId,
    hash: &str,
    voice: &Voice,
    text: &str,
    audio: &Bytes,
) -> Result<(), AppError> {
    let storage_key = format!("tenants/{tenant}/tts/{hash}.ulaw");
    if let Err(e) = storage
        .put(&storage_key, audio.to_vec(), "audio/basic")
        .await
    {
        tracing::warn!(error = %e, "could not store cached audio");
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO tts_cache (tenant_id, hash, model, speaker, language, text, storage_key, byte_len, duration_ms) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (tenant_id, hash) DO NOTHING",
    )
    .bind(tenant.0)
    .bind(hash)
    .bind(&voice.model)
    .bind(&voice.speaker)
    .bind(&voice.language)
    .bind(text)
    .bind(&storage_key)
    .bind(audio.len() as i32)
    .bind((audio::mulaw_duration_ms(audio.len())) as i32)
    .execute(db)
    .await?;
    Ok(())
}

impl LineAudio for LineCache {
    fn get(&self, voice: &Voice, text: &str) -> Option<Bytes> {
        self.memory.get(&key(voice, text))
    }

    fn learn(&self, voice: &Voice, text: &str, audio: Bytes) {
        let hash = key(voice, text);
        if self.memory.contains_key(&hash) {
            return;
        }
        self.memory.insert(hash.clone(), audio.clone());
        metrics::counter!("voice_tts_phrases_learned_total").increment(1);
        let (db, storage) = (self.db.clone(), self.storage.clone());
        let (voice, text) = (voice.clone(), text.to_owned());
        tokio::spawn(async move {
            if let Err(e) =
                persist(&db, &storage, DEFAULT_TENANT, &hash, &voice, &text, &audio).await
            {
                tracing::warn!(error = %e, "could not save learned phrase");
            }
        });
    }
}
