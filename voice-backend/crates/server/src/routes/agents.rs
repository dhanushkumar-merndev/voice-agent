//! `/v1/agents`: create, list, read, edit and delete agents, and their
//! training notes (train.md) and lessons learned from calls.

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use common::{
    ids::{AgentId, LessonId, DEFAULT_TENANT},
    AppError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    services::{
        agents::{self, Agent, AgentInput},
        prompt_edit::{self, EditRequest, EditResult},
        training::{self, Lesson},
    },
    state::AppState,
};

fn parse(body: &[u8]) -> Result<AgentInput, AppError> {
    serde_json::from_slice(body)
        .map_err(|e| AppError::bad_request(format!("invalid JSON body: {e}")))
}

/// `GET /v1/agents`: every agent with its call count and last call time.
pub async fn list_agents(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let rows: Vec<Agent> =
        sqlx::query_as("SELECT * FROM agents WHERE tenant_id = $1 ORDER BY created_at DESC")
            .bind(DEFAULT_TENANT.0)
            .fetch_all(&state.db)
            .await
            .map_err(AppError::from)?;
    type Usage = (
        uuid::Uuid,
        i64,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<f64>,
        i64,
    );
    let usage: Vec<Usage> = sqlx::query_as(
        "SELECT agent_id, count(*), max(created_at), avg(review_score)::float8, \
           (SELECT count(*) FROM agent_lessons l WHERE l.agent_id = c.agent_id AND l.status = 'suggested') \
         FROM calls c WHERE tenant_id = $1 AND agent_id IS NOT NULL GROUP BY agent_id",
    )
    .bind(DEFAULT_TENANT.0)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::from)?;
    Ok(Json(json!({
        "agents": rows.iter().map(|a| {
            let used = usage.iter().find(|(id, ..)| *id == a.id);
            let mut v = serde_json::to_value(a).unwrap_or(Value::Null);
            v["calls"] = json!(used.map_or(0, |u| u.1));
            v["last_call_at"] = json!(used.and_then(|u| u.2));
            v["avg_review_score"] = json!(used.and_then(|u| u.3).map(|s| (s * 10.0).round() / 10.0));
            v["suggested_lessons"] = json!(used.map_or(0, |u| u.4));
            v
        }).collect::<Vec<_>>(),
    })))
}

/// `POST /v1/agents`.
pub async fn create_agent(
    State(state): State<AppState>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Agent>)> {
    let input = agents::validate(&state, DEFAULT_TENANT, parse(&body)?).await?;
    let id = AgentId::new();
    let agent: Agent = sqlx::query_as(
        "INSERT INTO agents (id, tenant_id, name, description, prompt, greeting, kb_id, language, \
           voice, booking, max_duration_secs, fields) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING *",
    )
    .bind(id.0)
    .bind(DEFAULT_TENANT.0)
    .bind(&input.name)
    .bind(&input.description)
    .bind(&input.prompt)
    .bind(&input.greeting)
    .bind(input.knowledge_base_id.map(|k| k.0))
    .bind(input.language.as_deref().unwrap_or("auto"))
    .bind(&input.voice)
    .bind(input.booking)
    .bind(input.max_duration_secs.map(|d| d as i32))
    .bind(sqlx::types::Json(&input.fields))
    .fetch_one(&state.db)
    .await
    .map_err(AppError::from)?;
    Ok((StatusCode::CREATED, Json(agent)))
}

/// `GET /v1/agents/{id}`.
pub async fn get_agent(
    State(state): State<AppState>,
    Path(id): Path<AgentId>,
) -> ApiResult<Json<Agent>> {
    Ok(Json(agents::get(&state, DEFAULT_TENANT, id).await?))
}

/// `PUT /v1/agents/{id}`: replaces the agent's settings. Calls already
/// placed keep the field list they started with.
pub async fn update_agent(
    State(state): State<AppState>,
    Path(id): Path<AgentId>,
    body: Bytes,
) -> ApiResult<Json<Agent>> {
    let input = agents::validate(&state, DEFAULT_TENANT, parse(&body)?).await?;
    let agent: Option<Agent> = sqlx::query_as(
        "UPDATE agents SET name = $3, description = $4, prompt = $5, greeting = $6, kb_id = $7, \
           language = $8, voice = $9, booking = $10, max_duration_secs = $11, fields = $12, \
           updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 RETURNING *",
    )
    .bind(id.0)
    .bind(DEFAULT_TENANT.0)
    .bind(&input.name)
    .bind(&input.description)
    .bind(&input.prompt)
    .bind(&input.greeting)
    .bind(input.knowledge_base_id.map(|k| k.0))
    .bind(input.language.as_deref().unwrap_or("auto"))
    .bind(&input.voice)
    .bind(input.booking)
    .bind(input.max_duration_secs.map(|d| d as i32))
    .bind(sqlx::types::Json(&input.fields))
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::from)?;
    agent
        .map(Json)
        .ok_or_else(|| ApiError(AppError::not_found("agent not found")))
}

/// `DELETE /v1/agents/{id}`: past calls keep their transcripts and fields.
pub async fn delete_agent(
    State(state): State<AppState>,
    Path(id): Path<AgentId>,
) -> ApiResult<StatusCode> {
    let done = sqlx::query("DELETE FROM agents WHERE id = $1 AND tenant_id = $2")
        .bind(id.0)
        .bind(DEFAULT_TENANT.0)
        .execute(&state.db)
        .await
        .map_err(AppError::from)?;
    if done.rows_affected() == 0 {
        return Err(ApiError(AppError::not_found("agent not found")));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /v1/agents/{id}/training`: the agent's train.md, its auto-apply
/// setting and the lessons shared by every agent (`prompts/train.md`).
pub async fn get_training(
    State(state): State<AppState>,
    Path(id): Path<AgentId>,
) -> ApiResult<Json<Value>> {
    let agent = agents::get(&state, DEFAULT_TENANT, id).await?;
    Ok(Json(json!({
        "training_notes": agent.training_notes,
        "auto_apply_lessons": agent.auto_apply_lessons,
        "max_chars": training::MAX_NOTES_CHARS,
        "shared_notes": state.assets.train,
    })))
}

/// `PUT /v1/agents/{id}/training` body; omitted fields stay as they are.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrainingInput {
    /// The whole train.md.
    #[serde(default)]
    training_notes: Option<String>,
    /// Apply suggested lessons without approval.
    #[serde(default)]
    auto_apply_lessons: Option<bool>,
}

/// `PUT /v1/agents/{id}/training`: replaces the train.md and/or the
/// auto-apply setting.
pub async fn put_training(
    State(state): State<AppState>,
    Path(id): Path<AgentId>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let input: TrainingInput = serde_json::from_slice(&body)
        .map_err(|e| AppError::bad_request(format!("invalid JSON body: {e}")))?;
    let notes = input
        .training_notes
        .as_deref()
        .map(training::clean_notes)
        .transpose()?;
    let row: Option<(String, bool)> = sqlx::query_as(
        "UPDATE agents SET training_notes = COALESCE($3, training_notes), \
           auto_apply_lessons = COALESCE($4, auto_apply_lessons), updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 RETURNING training_notes, auto_apply_lessons",
    )
    .bind(id.0)
    .bind(DEFAULT_TENANT.0)
    .bind(notes)
    .bind(input.auto_apply_lessons)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::from)?;
    let (notes, auto) = row.ok_or_else(|| ApiError(AppError::not_found("agent not found")))?;
    Ok(Json(json!({
        "training_notes": notes,
        "auto_apply_lessons": auto,
        "max_chars": training::MAX_NOTES_CHARS,
        "shared_notes": state.assets.train,
    })))
}

/// `GET /v1/agents/{id}/train.md`: the training notes as a Markdown file.
pub async fn download_training(
    State(state): State<AppState>,
    Path(id): Path<AgentId>,
) -> ApiResult<impl IntoResponse> {
    let agent = agents::get(&state, DEFAULT_TENANT, id).await?;
    let body = if agent.training_notes.trim().is_empty() {
        format!("# {} train.md\n\n## Lessons from calls\n", agent.name)
    } else {
        agent.training_notes
    };
    Ok((
        [
            (header::CONTENT_TYPE, "text/markdown; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"train.md\"",
            ),
        ],
        body,
    ))
}

/// `GET /v1/agents/{id}/lessons` query.
#[derive(Debug, Deserialize)]
pub struct LessonQuery {
    /// `suggested`, `applied` or `rejected`.
    status: Option<String>,
}

/// `GET /v1/agents/{id}/lessons`: lessons learned from calls, newest first.
pub async fn list_lessons(
    State(state): State<AppState>,
    Path(id): Path<AgentId>,
    Query(q): Query<LessonQuery>,
) -> ApiResult<Json<Value>> {
    if let Some(s) = q.status.as_deref() {
        if !["suggested", "applied", "rejected"].contains(&s) {
            return Err(ApiError(AppError::bad_request(
                "`status` must be suggested, applied or rejected",
            )));
        }
    }
    agents::get(&state, DEFAULT_TENANT, id).await?;
    let lessons = training::list(&state, DEFAULT_TENANT, id, q.status.as_deref()).await?;
    Ok(Json(json!({ "lessons": lessons })))
}

/// `POST /v1/agents/{id}/lessons/{lesson}/apply` body.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyInput {
    /// Reworded lesson; the suggestion as written when omitted.
    #[serde(default)]
    lesson: Option<String>,
}

/// `POST /v1/agents/{id}/lessons/{lesson}/apply`: adds the lesson to the
/// agent's train.md.
pub async fn apply_lesson(
    State(state): State<AppState>,
    Path((id, lesson)): Path<(AgentId, LessonId)>,
    body: Bytes,
) -> ApiResult<Json<Lesson>> {
    let input: ApplyInput = if body.is_empty() {
        ApplyInput::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| AppError::bad_request(format!("invalid JSON body: {e}")))?
    };
    Ok(Json(
        training::apply(&state, DEFAULT_TENANT, id, lesson, input.lesson.as_deref()).await?,
    ))
}

/// `POST /v1/agents/{id}/lessons/{lesson}/reject`.
pub async fn reject_lesson(
    State(state): State<AppState>,
    Path((id, lesson)): Path<(AgentId, LessonId)>,
) -> ApiResult<Json<Lesson>> {
    Ok(Json(
        training::reject(&state, DEFAULT_TENANT, id, lesson).await?,
    ))
}

/// `POST /v1/agents/prompt-edit`: the prompt rewritten by an LLM to make
/// the change described in `instruction`. Nothing is saved.
pub async fn edit_prompt(
    State(state): State<AppState>,
    body: Bytes,
) -> ApiResult<Json<EditResult>> {
    let req: EditRequest = serde_json::from_slice(&body)
        .map_err(|e| AppError::bad_request(format!("invalid JSON body: {e}")))?;
    Ok(Json(prompt_edit::edit(&state, req).await?))
}
