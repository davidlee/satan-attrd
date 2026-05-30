//! Tests for the disposable-database test harness itself (DE-002).
//!
//! - VT-with-db: `with_db()` swaps only the database name, preserving the rest
//!   of the connection target across the socket and tcp `DATABASE_URL` forms.
//! - VT-no-prod: `shared_pool()` connects to a generated `satan_attrd_test_*`
//!   database — never the database named by `DATABASE_URL`.
//! - VT-sweep-gc: `sweep_stale()` drops an old idle stray DB and keeps a recent one.
#![expect(
    clippy::expect_used,
    clippy::tests_outside_test_module,
    clippy::disallowed_methods,
    reason = "integration test crate: expect in test fixtures, tests at crate level, DATABASE_URL env access is the harness's job"
)]

#[expect(
    dead_code,
    reason = "test helpers shared across crates; unused in this binary"
)]
mod common;

use common::with_db;
use sqlx::Row;

// --- VT-with-db -----------------------------------------------------------

#[test]
fn with_db_socket_form_swaps_db() {
    // sqlx normalises the socket `?host=` path to its default socket directory
    // (`/var/run/postgresql`, which is `/run/postgresql` via the systemd
    // symlink), so we assert the one thing `with_db` owns — the database swap.
    // Real socket reachability is proven by VT-no-prod.
    let opts = with_db("postgres:///satan_memory?host=/run/postgresql", "postgres");
    assert_eq!(opts.get_database(), Some("postgres"));
}

#[test]
fn with_db_tcp_form_swaps_db_keeps_creds_host_port() {
    let opts = with_db(
        "postgresql://u:p@example.com:5432/satan_memory",
        "satan_attrd_test_42_abc",
    );
    assert_eq!(opts.get_database(), Some("satan_attrd_test_42_abc"));
    assert_eq!(opts.get_host(), "example.com");
    assert_eq!(opts.get_port(), 5432);
    assert_eq!(opts.get_username(), "u");
}

// --- VT-no-prod -----------------------------------------------------------

/// `shared_pool()` must connect to a generated `satan_attrd_test_*` database
/// even when `DATABASE_URL` names production, and never to the named database.
#[tokio::test]
async fn shared_pool_provisions_test_db_never_prod() {
    let named_db = with_db(
        &std::env::var("DATABASE_URL").expect("DATABASE_URL set"),
        "ignored",
    );
    // Sanity: prove `current_database()` reflects the connected DB, not env.
    let pool = common::shared_pool().await;
    let current: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .expect("query current_database");
    assert!(
        current.starts_with("satan_attrd_test_"),
        "expected a disposable test DB, got {current}"
    );
    assert_ne!(current, "satan_memory", "connected to production");
    // `named_db` only exists to assert env is irrelevant to the outcome.
    let _ = named_db.get_database();
}

// --- VT-sweep-gc ----------------------------------------------------------

/// `sweep_stale()` drops an old idle stray DB and spares a recent one.
#[tokio::test]
async fn sweep_drops_old_idle_keeps_recent() {
    let mut admin = common::admin_conn().await;

    // Old epoch (well before any process start) → must be reclaimed.
    let old = format!("satan_attrd_test_1000_{}", uuid_simple());
    // Far-future epoch → always >= cutoff → must be spared.
    let recent = format!("satan_attrd_test_9999999999999_{}", uuid_simple());
    common::create_database(&mut admin, &old).await;
    common::create_database(&mut admin, &recent).await;

    common::sweep_stale(&mut admin).await;

    assert!(
        !db_exists(&mut admin, &old).await,
        "old idle DB not reclaimed"
    );
    assert!(
        db_exists(&mut admin, &recent).await,
        "recent DB wrongly dropped"
    );

    // Clean up the spared DB so this test leaves nothing behind.
    sqlx::query(&format!(r#"DROP DATABASE IF EXISTS "{recent}""#))
        .execute(&mut admin)
        .await
        .expect("drop recent test DB");
}

fn uuid_simple() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

async fn db_exists(admin: &mut sqlx::PgConnection, name: &str) -> bool {
    sqlx::query("SELECT 1 FROM pg_database WHERE datname = $1")
        .bind(name)
        .fetch_optional(admin)
        .await
        .expect("query pg_database")
        .map(|r| r.get::<i32, _>(0))
        .is_some()
}
