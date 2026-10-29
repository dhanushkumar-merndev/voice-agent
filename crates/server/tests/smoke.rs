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

fn uuid_v7() -> uuid::Uuid {
    uuid::Uuid::now_v7()
}
