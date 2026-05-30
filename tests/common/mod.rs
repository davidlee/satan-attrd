//! Shared test harness for satan-attrd integration tests.
//!
//! `$DATABASE_URL` is treated as a **server pointer** only — its database
//! component is ignored. Each test self-provisions a throwaway database
//! `satan_attrd_test_<proc_start_ms>_<uuid>` from an admin connection (the same
//! server, database `postgres`), asserts it is connected to that database
//! (never the one `DATABASE_URL` names), migrates+seeds it, and runs there.
//! Production can never be a write target; a panicking test leaves no orphaned
//! rows because its whole database is disposable. Databases left by prior runs
//! are reclaimed by a lock-free, age-filtered sweep on first use (DE-002).
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::str::FromStr;
use std::sync::{LazyLock, Once};
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection, PgPool};
use tokio::sync::OnceCell;
use uuid::Uuid;

use satan_attrd::migrate;

static INIT_LOG: Once = Once::new();

/// Prefix shared by every disposable test database.
const TEST_DB_PREFIX: &str = "satan_attrd_test_";

/// Age cushion for the sweep: databases created within this window of process
/// start are spared, covering clock granularity and concurrent runs. Anything
/// missed is reclaimed by the next run.
const SWEEP_MARGIN_MS: u64 = 60_000;

/// Process start, captured once — the sweep's age cutoff and the epoch embedded
/// in every database this process creates.
static PROC_START_MS: LazyLock<u64> = LazyLock::new(epoch_ms);

/// The stale-database sweep runs at most once per test process.
static SWEPT: OnceCell<()> = OnceCell::const_new();

fn epoch_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before the unix epoch")
            .as_millis(),
    )
    .expect("epoch milliseconds exceed u64")
}

/// Parse the embedded epoch from a `satan_attrd_test_<epoch>_<uuid>` name.
/// `None` for any name not matching the shape (left untouched by the sweep).
fn epoch_of(db: &str) -> Option<u64> {
    db.strip_prefix(TEST_DB_PREFIX)?
        .split_once('_')
        .and_then(|(epoch, _uuid)| epoch.parse().ok())
}

/// Connection options for `url` with only the database name replaced by `db`.
///
/// Host/socket, credentials, port and query parameters are preserved. Parsing
/// goes through sqlx's `PgConnectOptions` (rather than string surgery) so the
/// socket form `postgres:///x?host=/run/postgresql` is handled correctly, and
/// the result is returned as options — not re-encoded to a URL — to avoid a
/// lossy round-trip.
#[must_use]
pub fn with_db(url: &str, db: &str) -> PgConnectOptions {
    PgConnectOptions::from_str(url)
        .expect("DATABASE_URL must be a valid Postgres connection string")
        .database(db)
}

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

/// Admin connection to the maintenance database (`postgres`) on the same
/// server `DATABASE_URL` points at. Used to create and reclaim test databases.
pub async fn admin_conn() -> PgConnection {
    let base = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set (server pointer; its db component is ignored)");
    PgConnection::connect_with(&with_db(&base, "postgres"))
        .await
        .unwrap()
}

/// Drop idle test databases left by **prior** runs. Lock-free: a database is
/// dropped only if its embedded epoch predates this process's start (minus a
/// margin), so a concurrent run's databases — which carry a recent epoch — are
/// spared. A `DROP` against a database with live connections fails (55006) and
/// is ignored; that database is reclaimed by a later run instead.
pub async fn sweep_stale(admin: &mut PgConnection) {
    let cutoff = PROC_START_MS.saturating_sub(SWEEP_MARGIN_MS);
    let dbs: Vec<String> = sqlx::query_scalar(
        "SELECT datname FROM pg_database WHERE datname LIKE 'satan_attrd_test_%'",
    )
    .fetch_all(&mut *admin)
    .await
    .unwrap();
    for db in dbs {
        if epoch_of(&db).is_some_and(|ms| ms < cutoff) {
            // `db` is a server-generated name (prefix + epoch + uuid); safe to
            // interpolate. Identifiers cannot be bound as parameters.
            let _ = sqlx::query(&format!(r#"DROP DATABASE IF EXISTS "{db}""#))
                .execute(&mut *admin)
                .await;
        }
    }
}

/// `CREATE DATABASE <name> TEMPLATE template0`. `template0` avoids inheriting
/// local `template1` mutations and never fails on `template1` having sessions.
/// A missing-CREATEDB privilege failure (SQLSTATE 42501) panics with an
/// explicit, actionable message rather than a cryptic sqlx error.
pub async fn create_database(admin: &mut PgConnection, name: &str) {
    // `name` is locally generated (prefix + epoch + uuid); safe to interpolate.
    let sql = format!(r#"CREATE DATABASE "{name}" TEMPLATE template0"#);
    let Err(e) = sqlx::query(&sql).execute(&mut *admin).await else {
        return;
    };
    match e
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .as_deref()
    {
        Some("42501") => panic!(
            "role lacks CREATEDB: cannot create test database {name}. Point \
             DATABASE_URL at a server/role that can create databases, or set \
             up a dedicated test role."
        ),
        _ => panic!("CREATE DATABASE {name} failed: {e}"),
    }
}

/// Provision a fresh disposable database and return a pool bound to it.
///
/// Called once per `#[tokio::test]` (a static pool would outlive the per-test
/// tokio runtime). Each call: derives an admin connection from `DATABASE_URL`
/// (database → `postgres`), sweeps prior-run strays once per process, creates
/// `satan_attrd_test_<proc_start_ms>_<uuid>`, connects, **asserts the live
/// `current_database()` is that name** before any write, then migrates+seeds.
pub async fn shared_pool() -> PgPool {
    init_log();
    let base = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set (server pointer; its db component is ignored)");

    let mut admin = admin_conn().await;
    SWEPT
        .get_or_init(|| async { sweep_stale(&mut admin).await })
        .await;

    let name = format!(
        "{TEST_DB_PREFIX}{}_{}",
        *PROC_START_MS,
        Uuid::new_v4().simple()
    );
    create_database(&mut admin, &name).await;

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(with_db(&base, &name))
        .await
        .unwrap();

    // Hard guard: a botched `with_db` could connect us elsewhere — the name
    // prefix alone is not proof. Assert the live connection's actual database.
    let current: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        current, name,
        "refusing to run: connected to {current}, not the disposable test DB"
    );

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
