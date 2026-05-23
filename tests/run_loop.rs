//! End-to-end smoke for the daemon run loop (T-attr-1c slice 2).
//!
//! Each test:
//!   1. Inserts a payload onto `satan_outcome_inbox`.
//!   2. Drives `RunLoop::drain_outcome_inbox` (foreground; equivalent to one
//!      LISTEN notification round-trip).
//!   3. Asserts on `satan_attribute_events`, `satan_attributes`, and
//!      `satan_audit_inbox` (the broker-side LISTEN consumer in the broker
//!      tests).
//!   4. Cleans up by `run_id`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{cleanup_run, shared_pool, unique_run_id};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::types::Json;
use tokio::sync::Mutex;

use satan_attrd::run_loop::RunLoop;

// Run loop tests touch the global projection rows (scope='global', name in
// the 8 attrs). Parallel tests would race those rows. The async mutex
// serializes the suite — `--test-threads=1` would also work but the mutex
// keeps the constraint visible in-file.
static PROJECTION_LOCK: Mutex<()> = Mutex::const_new(());

async fn reset_projection(pool: &PgPool) {
    sqlx::query("UPDATE satan_attributes SET value = 0.0 WHERE scope = 'global'")
        .execute(pool)
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

async fn enqueue_outcome(pool: &PgPool, payload: &Value) -> i32 {
    let (id,): (i32,) = sqlx::query_as(
        "INSERT INTO satan_outcome_inbox (payload_json)
         VALUES ($1)
         RETURNING id",
    )
    .bind(Json(payload))
    .fetch_one(pool)
    .await
    .unwrap();
    id
}

async fn audit_payloads_for_run(pool: &PgPool, run_id: &str) -> Vec<Value> {
    let rows: Vec<(Json<Value>,)> = sqlx::query_as(
        "SELECT payload_json FROM satan_audit_inbox
          WHERE payload_json->>'id' LIKE $1
          ORDER BY id",
    )
    .bind(format!("{run_id}.attr%"))
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter().map(|(Json(v),)| v).collect()
}

async fn event_count_for_run(pool: &PgPool, run_id: &str) -> i64 {
    let (n,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM satan_attribute_events WHERE run_id = $1")
            .bind(run_id)
            .fetch_one(pool)
            .await
            .unwrap();
    n
}

async fn cleanup(pool: &PgPool, run_id: &str) {
    cleanup_run(pool, run_id).await;
    sqlx::query(
        "DELETE FROM satan_audit_inbox WHERE payload_json->>'id' LIKE $1",
    )
    .bind(format!("{run_id}.attr%"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("DELETE FROM satan_outcome_inbox WHERE payload_json->>'run_id' = $1")
        .bind(run_id)
        .execute(pool)
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn outcome_inbox_to_audit_inbox_end_to_end_contradicted_medium() {
    let pool = shared_pool().await;
    let _g = PROJECTION_LOCK.lock().await;
    reset_projection(&pool).await;
    let run_id = unique_run_id();
    let iv_id = format!("{run_id}.iv001");

    let payload = json!({
        "schema_version": "1.0",
        "run_id": run_id,
        "ts": "2026-05-24T12:00:00Z",
        "intervention_id": iv_id,
        "classification": "contradicted",
        "confidence": "medium",
        "evidence": {
            "intervention_kind": "ask",
            "related_motive_id": null,
            "cue_handles": ["focus:tab-loss"],
            "related_trace_ids": []
        },
        "is_revision": false,
        "revises": null,
        "enabled": true,
    });
    let inbox_id = enqueue_outcome(&pool, &payload).await;

    let mut rl = RunLoop::new(pool.clone());
    rl.drain_outcome_inbox().await.unwrap();

    // contradicted affects: friction, shame, doubt, suspicion, metamorphosis (5).
    assert_eq!(event_count_for_run(&pool, &run_id).await, 5);

    let audits = audit_payloads_for_run(&pool, &run_id).await;
    assert_eq!(audits.len(), 5);
    for a in &audits {
        assert_eq!(a["schema_version"], json!("1.0"));
        assert_eq!(a["source"], json!("outcome"));
        assert_eq!(a["reason"], json!("contradicted"));
        assert_eq!(a["scope"], json!("global"));
        assert_eq!(a["disabled"], json!(false));
        assert_eq!(a["evidence"]["intervention_id"], json!(iv_id));
        assert_eq!(a["evidence"]["confidence"], json!("medium"));
        assert!(a["id"].as_str().unwrap().starts_with(&run_id));
    }

    // satan_outcome_inbox row consumed (deleted).
    let pending: Vec<(i32,)> =
        sqlx::query_as("SELECT id FROM satan_outcome_inbox WHERE id = $1")
            .bind(inbox_id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(pending.is_empty(), "outcome inbox row not deleted");

    cleanup(&pool, &run_id).await;
}

#[tokio::test]
async fn outcome_inbox_disabled_does_not_upsert_projection() {
    let pool = shared_pool().await;
    let _g = PROJECTION_LOCK.lock().await;
    reset_projection(&pool).await;
    let run_id = unique_run_id();
    let iv_id = format!("{run_id}.iv001");

    // Pre-read live shame value so we can assert it does NOT change.
    let (shame_before,): (f64,) =
        sqlx::query_as("SELECT value FROM satan_attributes WHERE scope='global' AND name='shame'")
            .fetch_one(&pool)
            .await
            .unwrap();

    let payload = json!({
        "schema_version": "1.0",
        "run_id": run_id,
        "ts": "2026-05-24T12:00:00Z",
        "intervention_id": iv_id,
        "classification": "harmful",
        "confidence": "high",
        "evidence": {
            "cue_handles": [],
            "related_trace_ids": []
        },
        "is_revision": false,
        "revises": null,
        "enabled": false,  // disabled — event must record but projection must not move
    });
    enqueue_outcome(&pool, &payload).await;

    let mut rl = RunLoop::new(pool.clone());
    rl.drain_outcome_inbox().await.unwrap();

    // Event rows still written (4 affected by harmful: friction, shame, doubt, metamorphosis).
    assert_eq!(event_count_for_run(&pool, &run_id).await, 4);

    // All events marked disabled.
    let (disabled_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM satan_attribute_events WHERE run_id = $1 AND disabled = true",
    )
    .bind(&run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(disabled_count, 4);

    // Projection unchanged.
    let (shame_after,): (f64,) =
        sqlx::query_as("SELECT value FROM satan_attributes WHERE scope='global' AND name='shame'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        (shame_after - shame_before).abs() < 1e-9,
        "disabled outcome upserted projection: {shame_before} -> {shame_after}"
    );

    // Audit payloads still enqueued (broker writes transcript line even for disabled events).
    let audits = audit_payloads_for_run(&pool, &run_id).await;
    assert_eq!(audits.len(), 4);
    for a in &audits {
        assert_eq!(a["disabled"], json!(true));
    }

    cleanup(&pool, &run_id).await;
}

#[tokio::test]
async fn outcome_inbox_rejects_bad_schema_version() {
    let pool = shared_pool().await;
    let _g = PROJECTION_LOCK.lock().await;
    reset_projection(&pool).await;
    let run_id = unique_run_id();
    let payload = json!({
        "schema_version": "2.0",  // future major — daemon rejects
        "run_id": run_id,
        "ts": "2026-05-24T12:00:00Z",
        "intervention_id": format!("{run_id}.iv001"),
        "classification": "worked",
        "confidence": "high",
        "evidence": { "cue_handles": [], "related_trace_ids": [] },
        "is_revision": false,
        "enabled": true,
    });
    let inbox_id = enqueue_outcome(&pool, &payload).await;

    let mut rl = RunLoop::new(pool.clone());
    rl.drain_outcome_inbox().await.unwrap();

    // No events; inbox row dropped.
    assert_eq!(event_count_for_run(&pool, &run_id).await, 0);
    let pending: Vec<(i32,)> =
        sqlx::query_as("SELECT id FROM satan_outcome_inbox WHERE id = $1")
            .bind(inbox_id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(pending.is_empty());

    cleanup(&pool, &run_id).await;
}

#[tokio::test]
async fn outcome_inbox_revision_uses_actual_prior_deltas() {
    let pool = shared_pool().await;
    let _g = PROJECTION_LOCK.lock().await;
    reset_projection(&pool).await;
    let run_id = unique_run_id();
    let iv_id = format!("{run_id}.iv001");

    // First emit: contradicted/medium.
    let first = json!({
        "schema_version": "1.0",
        "run_id": run_id,
        "ts": "2026-05-24T12:00:00Z",
        "intervention_id": iv_id,
        "classification": "contradicted",
        "confidence": "medium",
        "evidence": { "cue_handles": [], "related_trace_ids": [] },
        "is_revision": false,
        "enabled": true,
    });
    enqueue_outcome(&pool, &first).await;

    let mut rl = RunLoop::new(pool.clone());
    rl.drain_outcome_inbox().await.unwrap();
    let first_events = event_count_for_run(&pool, &run_id).await;
    assert_eq!(first_events, 5);

    // Revise to worked/high.
    let revision = json!({
        "schema_version": "1.0",
        "run_id": run_id,
        "ts": "2026-05-24T12:01:00Z",
        "intervention_id": iv_id,
        "classification": "worked",
        "confidence": "high",
        "evidence": { "cue_handles": [], "related_trace_ids": [] },
        "is_revision": true,
        "revises": "intervention.outcome_classified",
        "enabled": true,
    });
    enqueue_outcome(&pool, &revision).await;
    rl.drain_outcome_inbox().await.unwrap();

    // Union of (contradicted, worked):
    //   contradicted: friction, shame, doubt, suspicion, metamorphosis
    //   worked:                shame, doubt, hunger, brooding
    // Union = friction, shame, doubt, hunger, suspicion, brooding, metamorphosis (7).
    let total = event_count_for_run(&pool, &run_id).await;
    assert_eq!(total, first_events + 7);

    // Revision events carry `revises` + `prior_actual` in evidence.
    let revision_rows: Vec<(Json<Value>,)> = sqlx::query_as(
        "SELECT evidence_json FROM satan_attribute_events
          WHERE run_id = $1 AND reason = 'worked'
          ORDER BY seq",
    )
    .bind(&run_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(revision_rows.len(), 7);
    for (Json(ev),) in &revision_rows {
        assert_eq!(
            ev["revises"],
            json!("intervention.outcome_classified"),
            "revises pointer missing"
        );
        assert!(ev["prior_actual"].is_number(), "prior_actual missing");
    }

    cleanup(&pool, &run_id).await;
}

#[tokio::test]
async fn audit_replies_log_drop_on_broker_reject() {
    let pool = shared_pool().await;
    let run_id = unique_run_id();

    // Synthesise a daemon-side audit row + a broker-side reject.
    let (inbox_id,): (i32,) = sqlx::query_as(
        "INSERT INTO satan_audit_inbox (payload_json)
         VALUES ($1)
         RETURNING id",
    )
    .bind(Json(&json!({
        "schema_version": "1.0",
        "id": format!("{run_id}.attr001"),
        "scope": "global",
        "name": "shame",
        "old": 0.0,
        "new": 0.0,
        "delta": 0.0,
        "source": "outcome",
        "reason": "neutral",
        "evidence": { "intervention_id": "x", "classification": "neutral", "confidence": "high" },
        "caps_applied": [],
        "disabled": false,
        "ts": "2026-05-24T12:00:00Z"
    })))
    .fetch_one(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO satan_audit_replies (inbox_id, error_msg)
         VALUES ($1, $2)",
    )
    .bind(inbox_id)
    .bind("validator: reason neutral mid-test reject")
    .execute(&pool)
    .await
    .unwrap();

    let mut rl = RunLoop::new(pool.clone());
    rl.drain_audit_replies().await.unwrap();

    // Reply row consumed.
    let (n,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM satan_audit_replies WHERE inbox_id = $1")
            .bind(inbox_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(n, 0, "audit reply row not consumed");

    sqlx::query("DELETE FROM satan_audit_inbox WHERE id = $1")
        .bind(inbox_id)
        .execute(&pool)
        .await
        .unwrap();
}
