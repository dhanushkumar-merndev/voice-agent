//! Spend that belongs to no single call (`spend_ledger`): phrase audio for
//! the shared cache and AI prompt edits. Per-call spend is on the call row.

use common::ids::TenantId;
use sqlx::PgPool;

use super::postcall::record_cost;

/// Kind of off-call spend.
#[derive(Debug, Clone, Copy)]
pub enum SpendKind {
    /// TTS for the shared phrase cache.
    TtsCache,
    /// "Edit with AI" on an agent prompt.
    PromptEdit,
}

impl SpendKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::TtsCache => "tts_cache",
            Self::PromptEdit => "prompt_edit",
        }
    }
}

/// Records `inr` of off-call spend. Failures are logged, never returned:
/// the work it paid for has already happened.
pub async fn record(
    db: &PgPool,
    tenant: TenantId,
    kind: SpendKind,
    model: &str,
    units: u64,
    inr: f64,
) {
    record_cost(kind.as_str(), model, inr);
    let result = sqlx::query(
        "INSERT INTO spend_ledger (id, tenant_id, kind, model, units, cost_inr) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(tenant.0)
    .bind(kind.as_str())
    .bind(model)
    .bind(i64::try_from(units).unwrap_or(i64::MAX))
    .bind(inr.max(0.0))
    .execute(db)
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, kind = kind.as_str(), inr, "could not record spend");
    }
}
