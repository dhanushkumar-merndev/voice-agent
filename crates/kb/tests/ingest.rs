//! Ingest a document with the real bge-m3 model and search it through
//! pgvector. Runs only when `TEST_DATABASE_URL` is set and the model has
//! been downloaded (`scripts/download_models.sh`).

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::{path::Path, sync::Arc};

use common::ids::{DocumentId, KbId, DEFAULT_TENANT};
use kb::{Embedder, KbService};

#[tokio::test]
async fn ingest_and_search_multilingual() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/bge-m3");
    if !dir.join("model_quantized.onnx").exists() {
        eprintln!("skipping: bge-m3 not downloaded");
        return;
    }
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    common::db::MIGRATOR.run(&db).await.unwrap();
    let embedder = Arc::new(Embedder::load(&dir, 4).unwrap());
    let service = KbService::new(db.clone(), Some(embedder));

    let kb = KbId::new();
    let doc = DocumentId::new();
    sqlx::query("INSERT INTO knowledge_bases (id, tenant_id, name) VALUES ($1, $2, 'clinic')")
        .bind(kb.0)
        .bind(DEFAULT_TENANT.0)
        .execute(&db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO kb_documents (id, tenant_id, kb_id, filename) VALUES ($1, $2, $3, 'faq.md')",
    )
    .bind(doc.0)
    .bind(DEFAULT_TENANT.0)
    .bind(kb.0)
    .execute(&db)
    .await
    .unwrap();

    let faq = "# Clinic Growth FAQ\n\n\
        ## Pricing\nOur Google and Facebook ads package costs 15,000 rupees per month, plus ad spend.\n\n\
        ## Booking system\nPatients book appointments on WhatsApp; reminders are sent automatically the day before.\n\n\
        ## Office hours\nThe support team is available Monday to Saturday, 10 AM to 7 PM.\n";
    let chunks = service
        .ingest(DEFAULT_TENANT, kb, doc, "faq.md", faq.as_bytes().to_vec())
        .await
        .unwrap();
    assert!(chunks >= 1);
    let (status,): (String,) = sqlx::query_as("SELECT status FROM kb_documents WHERE id = $1")
        .bind(doc.0)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(status, "ready");

    let started = std::time::Instant::now();
    let hits = service
        .search(kb, "विज्ञापन पैकेज की कीमत क्या है?", 3)
        .await
        .unwrap();
    let took = started.elapsed();
    assert!(!hits.is_empty());
    assert!(hits[0].content.contains("15,000"), "{hits:?}");
    eprintln!(
        "hindi query → top hit score {:.3} in {:?}",
        hits[0].score, took
    );
}
