//! `/v1/knowledge-bases`: create, upload documents, list, search.

use axum::{
    body::Bytes,
    extract::{Multipart, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use common::{
    ids::{DocumentId, KbId, DEFAULT_TENANT},
    AppError, ErrorCode,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    services::{
        idempotency::{self, Begin},
        util::sha256_hex,
    },
    state::AppState,
};

/// Largest accepted upload.
pub const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateKb {
    name: String,
}

/// `POST /v1/knowledge-bases`.
pub async fn create_kb(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let req: CreateKb = serde_json::from_slice(&body)
        .map_err(|e| AppError::bad_request(format!("invalid JSON body: {e}")))?;
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(ApiError(AppError::bad_request(
            "`name` must be 1-200 characters",
        )));
    }
    let claimed = match idempotency::begin(
        &state.db,
        DEFAULT_TENANT,
        "POST /v1/knowledge-bases",
        idempotency::header(&headers)?,
        &sha256_hex(&body),
    )
    .await?
    {
        Begin::Replay(status, body) => return Ok((status, Json(body)).into_response()),
        Begin::Fresh(k) => k,
    };
    let id = KbId::new();
    let result: Result<(DateTime<Utc>,), sqlx::Error> = sqlx::query_as(
        "INSERT INTO knowledge_bases (id, tenant_id, name) VALUES ($1, $2, $3) RETURNING created_at",
    )
    .bind(id.0)
    .bind(DEFAULT_TENANT.0)
    .bind(name)
    .fetch_one(&state.db)
    .await;
    match result {
        Ok((created_at,)) => {
            let body = json!({ "id": id, "name": name, "created_at": created_at });
            idempotency::finish(&state.db, claimed, StatusCode::CREATED, &body).await;
            Ok((StatusCode::CREATED, Json(body)).into_response())
        }
        Err(e) => {
            idempotency::abandon(&state.db, claimed).await;
            Err(ApiError(e.into()))
        }
    }
}

async fn ensure_kb(state: &AppState, kb: KbId) -> Result<(), AppError> {
    let row: Option<(uuid::Uuid,)> =
        sqlx::query_as("SELECT id FROM knowledge_bases WHERE id = $1 AND tenant_id = $2")
            .bind(kb.0)
            .bind(DEFAULT_TENANT.0)
            .fetch_optional(&state.db)
            .await?;
    row.map(|_| ())
        .ok_or_else(|| AppError::not_found("knowledge base not found"))
}

/// `POST /v1/knowledge-bases/{id}/documents` (multipart field `file`).
pub async fn upload_document(
    State(state): State<AppState>,
    Path(kb): Path<KbId>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<Response> {
    ensure_kb(&state, kb).await?;
    if !state.kb.available() {
        return Err(ApiError(AppError::new(
            ErrorCode::ProviderNotConfigured,
            "embedding model missing: run scripts/download_models.sh",
        )));
    }
    let mut file: Option<(String, Option<String>, Bytes)> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("invalid multipart body: {e}")))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let filename = field
            .file_name()
            .map(|f| f.rsplit(['/', '\\']).next().unwrap_or(f).to_owned())
            .filter(|f| !f.is_empty())
            .ok_or_else(|| AppError::bad_request("the `file` field needs a filename"))?;
        let content_type = field.content_type().map(str::to_owned);
        let data = field.bytes().await.map_err(|e| {
            AppError::new(
                ErrorCode::PayloadTooLarge,
                format!("could not read upload: {e}"),
            )
        })?;
        file = Some((filename, content_type, data));
        break;
    }
    let Some((filename, content_type, data)) = file else {
        return Err(ApiError(AppError::bad_request(
            "multipart field `file` is required",
        )));
    };
    kb::parse::kind_for(&filename)
        .map_err(|_| AppError::bad_request("only .txt, .md and .pdf files are supported"))?;

    let mut hash_input = filename.as_bytes().to_vec();
    hash_input.extend_from_slice(&data);
    let claimed = match idempotency::begin(
        &state.db,
        DEFAULT_TENANT,
        "POST /v1/knowledge-bases/documents",
        idempotency::header(&headers)?,
        &sha256_hex(&hash_input),
    )
    .await?
    {
        Begin::Replay(status, body) => return Ok((status, Json(body)).into_response()),
        Begin::Fresh(k) => k,
    };

    let doc = DocumentId::new();
    let key = format!("tenants/{DEFAULT_TENANT}/kb/{kb}/{doc}/{filename}");
    let stored = async {
        state
            .storage
            .put(
                &key,
                data.to_vec(),
                content_type.as_deref().unwrap_or("application/octet-stream"),
            )
            .await?;
        sqlx::query(
            "INSERT INTO kb_documents (id, tenant_id, kb_id, filename, content_type, size_bytes, storage_key, status) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending')",
        )
        .bind(doc.0)
        .bind(DEFAULT_TENANT.0)
        .bind(kb.0)
        .bind(&filename)
        .bind(&content_type)
        .bind(data.len() as i64)
        .bind(&key)
        .execute(&state.db)
        .await?;
        Ok::<_, AppError>(())
    }
    .await;
    if let Err(e) = stored {
        idempotency::abandon(&state.db, claimed).await;
        return Err(ApiError(e));
    }

    let service = state.kb.clone();
    let name = filename.clone();
    tokio::spawn(async move {
        let _ = service
            .ingest(DEFAULT_TENANT, kb, doc, &name, data.to_vec())
            .await;
    });
    let body = json!({ "document_id": doc, "filename": filename, "status": "pending" });
    idempotency::finish(&state.db, claimed, StatusCode::ACCEPTED, &body).await;
    Ok((StatusCode::ACCEPTED, Json(body)).into_response())
}

/// `GET /v1/knowledge-bases/{id}/documents`.
pub async fn list_documents(
    State(state): State<AppState>,
    Path(kb): Path<KbId>,
) -> ApiResult<Json<Value>> {
    ensure_kb(&state, kb).await?;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        uuid::Uuid,
        String,
        String,
        Option<String>,
        Option<i64>,
        DateTime<Utc>,
        i64,
    )> = sqlx::query_as(
        "SELECT d.id, d.filename, d.status, d.error, d.size_bytes, d.created_at, \
             (SELECT count(*) FROM kb_chunks c WHERE c.document_id = d.id) \
             FROM kb_documents d WHERE d.kb_id = $1 ORDER BY d.created_at DESC",
    )
    .bind(kb.0)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::from)?;
    Ok(Json(json!({
        "documents": rows.iter().map(|(id, filename, status, error, size, created, chunks)| json!({
            "id": id, "filename": filename, "status": status, "error": error,
            "size_bytes": size, "chunks": chunks, "created_at": created,
        })).collect::<Vec<_>>()
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchReq {
    query: String,
    #[serde(default)]
    top_k: Option<i64>,
}

/// `POST /v1/knowledge-bases/{id}/search` (debug).
pub async fn search(
    State(state): State<AppState>,
    Path(kb): Path<KbId>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let req: SearchReq = serde_json::from_slice(&body)
        .map_err(|e| AppError::bad_request(format!("invalid JSON body: {e}")))?;
    if req.query.trim().is_empty() {
        return Err(ApiError(AppError::bad_request("`query` is required")));
    }
    ensure_kb(&state, kb).await?;
    let started = std::time::Instant::now();
    let hits = state
        .kb
        .search(kb, &req.query, req.top_k.unwrap_or(3))
        .await
        .map_err(|e| AppError::new(ErrorCode::ProviderNotConfigured, e.to_string()))?;
    Ok(Json(json!({
        "results": hits,
        "took_ms": started.elapsed().as_millis() as u64,
    })))
}
