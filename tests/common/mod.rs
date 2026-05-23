//! Shared test harness for satan-attrd integration tests.
//!
//! Tests require Postgres reachable at `$DATABASE_URL`. The harness:
//!
//! - migrates the database (idempotent).
//! - hands tests a unique `run_id` so event-log writes never collide between
//!   parallel test cases.
//! - exposes raw-scope UPSERT / cleanup helpers so projection tests can use
//!   unique scope strings without expanding the production `Scope` enum.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::sync::Once;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use satan_attrd::migrate;

static INIT_LOG: Once = Once::new();

fn init_log() {
    INIT_LOG.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with_test_writer()
            .try_init()
            .ok();
    });
}

/// Per-test pool. `#[tokio::test]` builds a fresh runtime per test, so a
/// pool shared via a static `OnceCell` outlives the runtime that created it
/// and dies on the next test. Per-test pools sidestep the problem; migrate
/// is idempotent so paying for it on every test is cheap.
pub async fn shared_pool() -> PgPool {
    init_log();
    let url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set for satan-attrd integration tests");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    migrate::run_migrations(&pool).await.unwrap();
    pool
}

/// Unique per-test `run_id`. Format matches the broker's `<UTC>-<mode>-<entropy>`
/// shape closely enough that downstream readers won't choke.
#[must_use]
pub fn unique_run_id() -> String {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    format!("{stamp}-test-{}", Uuid::new_v4().simple())
}

/// Unique per-test scope string for projection isolation. The production
/// `Scope` enum only allows `Global`; tests use raw scope strings so parallel
/// tests don't fight over the 8 seeded global rows.
#[must_use]
pub fn unique_scope() -> String {
    format!("test:{}", Uuid::new_v4().simple())
}

/// Raw UPSERT bypassing the typed `Scope` enum — for tests that need to
/// write to a non-global scope. Production code uses
/// `satan_attrd::upsert_attribute` with `Scope::Global`.
pub async fn upsert_raw(
    pool: &PgPool,
    scope: &str,
    name: &str,
    value: f64,
    evidence_json: &serde_json::Value,
) {
    sqlx::query(
        "INSERT INTO satan_attributes (scope, name, value, updated_at, evidence_json)
         VALUES ($1, $2, $3, NOW(), $4)
         ON CONFLICT (scope, name)
         DO UPDATE SET value = EXCLUDED.value,
                       updated_at = EXCLUDED.updated_at,
                       evidence_json = EXCLUDED.evidence_json",
    )
    .bind(scope)
    .bind(name)
    .bind(value)
    .bind(sqlx::types::Json(evidence_json))
    .execute(pool)
    .await
    .unwrap();
}

pub async fn select_raw(
    pool: &PgPool,
    scope: &str,
    name: &str,
) -> Option<(f64, serde_json::Value)> {
    let row: Option<(f64, sqlx::types::Json<serde_json::Value>)> = sqlx::query_as(
        "SELECT value, evidence_json FROM satan_attributes WHERE scope = $1 AND name = $2",
    )
    .bind(scope)
    .bind(name)
    .fetch_optional(pool)
    .await
    .unwrap();
    row.map(|(v, e)| (v, e.0))
}

/// Delete every row this test created. Call from a test's tail or via a
/// `Drop` guard pattern; not auto-invoked.
pub async fn cleanup_scope(pool: &PgPool, scope: &str) {
    sqlx::query("DELETE FROM satan_attributes WHERE scope = $1")
        .bind(scope)
        .execute(pool)
        .await
        .unwrap();
}

pub async fn cleanup_run(pool: &PgPool, run_id: &str) {
    sqlx::query("DELETE FROM satan_attribute_events WHERE run_id = $1")
        .bind(run_id)
        .execute(pool)
        .await
        .unwrap();
}
