//! Smoke test against a real Postgres + pgvector container: migrations
//! apply, HNSW vector search works, and the router serves health, auth,
//! validation, idempotency and webhook-signature responses. Requires Docker.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::{path::Path, time::Duration};

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use common::{assets::Assets, ids::DEFAULT_TENANT, Config};
use serde_json::{json, Value};
use server::{app::build_router, bootstrap, observability};
use sqlx::{postgres::PgPoolOptions, PgPool};
use testcontainers::{
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
    ContainerAsync, GenericImage, ImageExt,
};
use tower::ServiceExt;

const API_KEY: &str = "test-key-0123456789";

async fn start_postgres() -> (ContainerAsync<GenericImage>, String) {
    let container = GenericImage::new("pgvector/pgvector", "pg16")
        .with_exposed_port(5432.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_USER", "voice")
        .with_env_var("POSTGRES_PASSWORD", "voice")
        .with_env_var("POSTGRES_DB", "voice")
        .start()
        .await
        .expect("start pgvector container (is Docker running?)");
    let port = container.get_host_port_ipv4(5432.tcp()).await.unwrap();
    (
        container,
        format!("postgres://voice:voice@127.0.0.1:{port}/voice"),
    )
}

/// The image restarts Postgres once after init, so retry the first connection.
async fn connect(url: &str) -> PgPool {
    for _ in 0..60 {
        if let Ok(pool) = PgPoolOptions::new().max_connections(4).connect(url).await {
            if sqlx::query("SELECT 1").execute(&pool).await.is_ok() {
                return pool;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("postgres did not become ready");
}

fn test_config(database_url: &str) -> Config {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let prompts = root.join("prompts").display().to_string();
    let config_dir = root.join("config").display().to_string();
    let database_url = database_url.to_owned();
    Config::from_lookup(move |key| {
        let v = match key {
            "PUBLIC_BASE_URL" => "http://localhost:8080",
            "API_KEY" => API_KEY,
            "DATABASE_URL" => database_url.as_str(),
            // Nothing listens here: storage readiness must report false.
            "S3_ENDPOINT" => "http://127.0.0.1:1",
            "S3_BUCKET" => "voice",
            "S3_ACCESS_KEY" => "minio",
            "S3_SECRET_KEY" => "minio123",
            "PROMPTS_DIR" => prompts.as_str(),
            "CONFIG_DIR" => config_dir.as_str(),
            "PROVIDER_MODE" => "fake",
            "MODELS_DIR" => "/nonexistent",
            _ => return None,
        };
        Some(v.to_owned())
    })
    .unwrap()
}

fn vector_literal(hot: usize) -> String {
    let mut v = vec![0.0_f32; 1024];
    v[hot] = 1.0;
    let parts: Vec<String> = v.iter().map(ToString::to_string).collect();
    format!("[{}]", parts.join(","))
}

async fn post(
    app: &Router,
    uri: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-api-key", API_KEY);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn get(app: &Router, uri: &str, api_key: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::builder().uri(uri);
    if let Some(key) = api_key {
        req = req.header("x-api-key", key);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn migrations_vector_search_and_router() {
    // TEST_DATABASE_URL points at an existing pgvector database (no Docker needed).
    let (_container, url) = match std::env::var("TEST_DATABASE_URL") {
        Ok(url) => (None, url),
        Err(_) => {
            let (c, url) = start_postgres().await;
            (Some(c), url)
        }
    };
    let pool = connect(&url).await;
    common::db::MIGRATOR
        .run(&pool)
        .await
        .expect("migrations apply");

    // --- pgvector: nearest neighbour through the HNSW cosine index ---------
    let kb = uuid_v7();
    let doc = uuid_v7();
    let seed_kb = kb;
    sqlx::query("INSERT INTO knowledge_bases (id, tenant_id, name) VALUES ($1, $2, 'test')")
        .bind(kb)
        .bind(DEFAULT_TENANT.0)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO kb_documents (id, tenant_id, kb_id, filename, status) \
         VALUES ($1, $2, $3, 'a.txt', 'ready')",
    )
    .bind(doc)
    .bind(DEFAULT_TENANT.0)
    .bind(kb)
    .execute(&pool)
    .await
    .unwrap();
    for (seq, hot, content) in [(0, 0, "pricing"), (1, 1, "timings")] {
        sqlx::query(
            "INSERT INTO kb_chunks (id, tenant_id, kb_id, document_id, seq, content, token_count, embedding) \
             VALUES ($1, $2, $3, $4, $5, $6, 3, $7::vector)",
        )
        .bind(uuid_v7())
        .bind(DEFAULT_TENANT.0)
        .bind(kb)
        .bind(doc)
        .bind(seq)
        .bind(content)
        .bind(vector_literal(hot))
        .execute(&pool)
        .await
        .unwrap();
    }
    let (content, score): (String, f64) = sqlx::query_as(
        "SELECT content, 1 - (embedding <=> $1::vector) AS score FROM kb_chunks \
         WHERE kb_id = $2 ORDER BY embedding <=> $1::vector LIMIT 1",
    )
    .bind(vector_literal(1))
    .bind(kb)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(content, "timings");
    assert!((score - 1.0).abs() < 1e-6);

    // --- router ------------------------------------------------------------
    let config = test_config(&url);
    let assets = Assets::load(&config.paths.prompts_dir, &config.paths.config_dir).unwrap();
    let pool_for_lists = pool.clone();
    let state = bootstrap::build_state(
        config,
        assets,
        pool,
        observability::test_metrics_handle().unwrap(),
    )
    .await;
    let lifecycle = state.lifecycle.clone();
    let app = build_router(state);

    let (status, body) = get(&app, "/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");

    let (status, body) = get(&app, "/v1/ping", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");

    let (status, _) = get(&app, "/v1/ping", Some("wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = get(&app, "/v1/ping", Some(API_KEY)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);

    let (status, body) = get(&app, "/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");

    // Database is up, storage is not: not ready, with per-check detail.
    let (status, body) = get(&app, "/readyz", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["checks"]["database"], true);
    assert_eq!(body["checks"]["storage"], false);

    // Knowledge base create + idempotent replay.
    let key = uuid::Uuid::now_v7().to_string();
    let (status, kb) = post(
        &app,
        "/v1/knowledge-bases",
        json!({"name": "Clinic"}),
        &[("idempotency-key", key.as_str())],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, again) = post(
        &app,
        "/v1/knowledge-bases",
        json!({"name": "Clinic"}),
        &[("idempotency-key", key.as_str())],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(kb["id"], again["id"]);
    let (status, body) = post(
        &app,
        "/v1/knowledge-bases",
        json!({"name": "Other"}),
        &[("idempotency-key", key.as_str())],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "conflict");

    // Call validation and provider checks (Plivo is not configured here).
    let (status, body) = post(
        &app,
        "/v1/calls",
        json!({"to": "12345", "lead_name": "Ravi", "system_prompt": "x"}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "bad_request");
    let (status, body) = post(
        &app,
        "/v1/calls",
        json!({"to": "+919876543210", "lead_name": "Ravi", "system_prompt": "x", "bogus": 1}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Plivo webhooks without a valid signature are rejected.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/plivo/answer/{}", uuid::Uuid::now_v7()))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("CallUUID=abc"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // Dashboard lists: calls (numbers masked), knowledge bases, stats, options.
    let call = uuid_v7();
    sqlx::query(
        "INSERT INTO calls (id, tenant_id, to_number, lead_name, system_prompt, status, \
           answered_at, ended_at, duration_secs, cost_inr, cost_breakdown, telephony_cost_inr) \
         VALUES ($1, $2, '+919876543210', 'Ravi', 'x', 'completed', now(), now(), 90, 1.5, \
           '{\"stt\": 0.5, \"tts\": 0.6, \"llm\": 0.4}', 0.65)",
    )
    .bind(call)
    .bind(DEFAULT_TENANT.0)
    .execute(&pool_for_lists)
    .await
    .unwrap();
    let (status, body) = get(&app, "/v1/calls?limit=10", Some(API_KEY)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 1);
    assert_eq!(body["calls"][0]["to"], "********3210");
    assert_eq!(body["calls"][0]["cost_excl_gst"], 2.15);
    let (status, body) = get(&app, "/v1/calls?status=busy", Some(API_KEY)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["calls"].as_array().map(Vec::len), Some(0));

    let (status, body) = get(&app, "/v1/stats?days=7", Some(API_KEY)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["totals"]["calls"], 1);
    assert_eq!(body["totals"]["talk_minutes"], 1.5);
    assert_eq!(body["cost"]["ai"]["tts"], 0.6);
    // The total also holds off-call spend (phrase audio warmed at start-up).
    let other = body["cost"]["other"]["total"].as_f64().unwrap();
    assert!(
        (body["cost"]["total_excl_gst"].as_f64().unwrap() - (2.15 + other)).abs() < 1e-3,
        "{body}"
    );
    assert_eq!(body["cost"]["per_answered_call_excl_gst"], 2.15);
    assert_eq!(body["daily"].as_array().map(Vec::len), Some(7));
    assert_eq!(body["by_status"][0]["key"], "completed");

    let (status, body) = get(&app, "/v1/options", Some(API_KEY)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["personas"].as_array().is_some_and(|p| !p.is_empty()));
    assert!(body["languages"].as_array().is_some_and(|l| !l.is_empty()));

    let (status, body) = get(&app, "/v1/knowledge-bases", Some(API_KEY)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed = body["knowledge_bases"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let seeded = listed
        .iter()
        .find(|k| k["id"] == json!(seed_kb))
        .expect("seeded kb listed");
    assert_eq!(seeded["documents"], 1);
    assert_eq!(seeded["chunks"], 2);
    // The seeded document has no stored file: chunks only, no text or link.
    let (status, body) = get(
        &app,
        &format!("/v1/knowledge-bases/{seed_kb}/documents/{doc}"),
        Some(API_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["filename"], "a.txt");
    assert_eq!(body["chunks"][0]["content"], "pricing");
    assert_eq!(body["chunks"][1]["seq"], 1);
    assert!(body["text"].is_null() && body["file_url"].is_null());
    let (status, _) = get(
        &app,
        &format!(
            "/v1/knowledge-bases/{seed_kb}/documents/{}",
            uuid::Uuid::new_v4()
        ),
        Some(API_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/v1/knowledge-bases/{seed_kb}/documents/{doc}"),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/v1/knowledge-bases/{seed_kb}/documents/{doc}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&app, "DELETE", &format!("/v1/knowledge-bases/{seed_kb}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = get(&app, "/v1/knowledge-bases", Some(API_KEY)).await;
    assert!(!body["knowledge_bases"]
        .as_array()
        .is_some_and(|l| l.iter().any(|k| k["id"] == json!(seed_kb))));

    // Agents: create, validate, list, edit, use on a call, delete.
    let agent_body = json!({
        "name": "Realty sales",
        "prompt": "You are Priya from Star Homes, calling about 2BHK flats in Mysuru.",
        "greeting": "I'm calling from Star Homes about flats. Is now a good time?",
        "language": "en-IN",
        "max_duration_secs": 120,
        "fields": [
            {"key": "city", "label": "City", "required": true},
            {"key": "budget_lakh", "label": "Budget (lakh)", "type": "number"},
            {"key": "bhk", "label": "BHK", "type": "choice", "options": ["1", "2", "3"]},
        ],
    });
    let (status, agent) = post(&app, "/v1/agents", agent_body.clone(), &[]).await;
    assert_eq!(status, StatusCode::CREATED, "{agent}");
    assert_eq!(agent["fields"][2]["options"], json!(["1", "2", "3"]));
    assert_eq!(agent["language"], "en-IN");
    let agent_id = agent["id"].as_str().unwrap().to_owned();
    for (bad, why) in [
        (json!({"name": "x", "prompt": ""}), "empty prompt"),
        (
            json!({"name": "x", "prompt": "p", "fields": [{"key": "City", "label": "c"}]}),
            "key case",
        ),
        (
            json!({"name": "x", "prompt": "p", "fields": [{"key": "a", "label": "a", "type": "choice", "options": ["one"]}]}),
            "one option",
        ),
        (
            json!({"name": "x", "prompt": "p", "fields": [{"key": "a", "label": "a"}, {"key": "a", "label": "b"}]}),
            "repeated key",
        ),
        (
            json!({"name": "x", "prompt": "p", "greeting": "g".repeat(150)}),
            "long greeting",
        ),
        (
            json!({"name": "x", "prompt": "p", "language": "kannada"}),
            "language",
        ),
    ] {
        let (status, body) = post(&app, "/v1/agents", bad, &[]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
    }
    let (status, body) = get(&app, "/v1/agents", Some(API_KEY)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["agents"][0]["id"], agent_id.as_str());
    assert_eq!(body["agents"][0]["calls"], 0);
    let mut edited = agent_body.clone();
    edited["name"] = json!("Realty sales v2");
    edited["fields"] = json!([{"key": "city", "label": "City"}]);
    let (status, body) = send_json(&app, "PUT", &format!("/v1/agents/{agent_id}"), edited).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "Realty sales v2");
    assert_eq!(body["fields"].as_array().map(Vec::len), Some(1));
    let (status, body) = get(&app, &format!("/v1/agents/{agent_id}"), Some(API_KEY)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "Realty sales v2");
    // An agent cannot be combined with a persona; an unknown agent is 404.
    let (status, body) = post(
        &app,
        "/v1/calls",
        json!({"to": "+919876543210", "lead_name": "Ravi", "agent_id": agent_id, "persona": "clinic_booking"}),
        &[("idempotency-key", "agent-and-persona")],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = post(
        &app,
        "/v1/calls",
        json!({"to": "+919876543210", "lead_name": "Ravi", "agent_id": uuid_v7()}),
        &[("idempotency-key", "unknown-agent")],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    // Training notes (train.md) and lessons.
    let training = format!("/v1/agents/{agent_id}/training");
    let (status, body) = get(&app, &training, Some(API_KEY)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["training_notes"], "");
    assert_eq!(body["auto_apply_lessons"], false);
    let (status, body) = send_json(
        &app,
        "PUT",
        &training,
        json!({"training_notes": "Quote no prices.\r\n"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["training_notes"], "Quote no prices.\n");
    let (status, body) = send_json(
        &app,
        "PUT",
        &training,
        json!({"training_notes": "x".repeat(8001)}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // Saving the agent form keeps the notes.
    let (status, body) = send_json(
        &app,
        "PUT",
        &format!("/v1/agents/{agent_id}"),
        agent_body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["training_notes"], "Quote no prices.\n");
    let lesson = uuid_v7();
    let rejected = uuid_v7();
    for id in [lesson, rejected] {
        sqlx::query(
            "INSERT INTO agent_lessons (id, tenant_id, agent_id, lesson, mistake, source) \
             VALUES ($1, $2, $3::uuid, 'Ask for the city once.', 'Asked the city twice.', 'review')",
        )
        .bind(id)
        .bind(common::ids::DEFAULT_TENANT.0)
        .bind(&agent_id)
        .execute(&pool_for_lists)
        .await
        .unwrap();
    }
    let (status, body) = get(
        &app,
        &format!("/v1/agents/{agent_id}/lessons?status=suggested"),
        Some(API_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["lessons"].as_array().map(Vec::len), Some(2));
    let (status, body) = post(
        &app,
        &format!("/v1/agents/{agent_id}/lessons/{lesson}/apply"),
        json!({"lesson": "Ask for the city only once."}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "applied");
    let (status, body) = post(
        &app,
        &format!("/v1/agents/{agent_id}/lessons/{lesson}/apply"),
        json!({}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "applied twice: {body}");
    let (status, body) = post(
        &app,
        &format!("/v1/agents/{agent_id}/lessons/{rejected}/reject"),
        json!({}),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "rejected");
    let (status, body) = get(&app, &training, Some(API_KEY)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["training_notes"],
        "Quote no prices.\n\n## Lessons from calls\n- Ask for the city only once.\n"
    );
    let (status, md) = send(&app, "GET", &format!("/v1/agents/{agent_id}/train.md")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&md).contains("- Ask for the city only once."));
    let (status, body) = get(&app, "/v1/agents", Some(API_KEY)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["agents"][0]["suggested_lessons"], 0);

    let (status, _) = send(&app, "DELETE", &format!("/v1/agents/{agent_id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = get(&app, &format!("/v1/agents/{agent_id}"), Some(API_KEY)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Unknown call.
    let (status, _) = get(
        &app,
        &format!("/v1/calls/{}", uuid::Uuid::now_v7()),
        Some(API_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    lifecycle.start_draining();
    let (status, body) = get(&app, "/readyz", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "draining");
}

/// Sends an authenticated JSON request with any method.
async fn send_json(app: &Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-api-key", API_KEY)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Sends a bodyless authenticated request.
async fn send(app: &Router, method: &str, uri: &str) -> (StatusCode, Vec<u8>) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("x-api-key", API_KEY)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, bytes.to_vec())
}

fn uuid_v7() -> uuid::Uuid {
    uuid::Uuid::now_v7()
}
