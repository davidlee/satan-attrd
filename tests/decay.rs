//! Decay scheduler integration tests.
//!
//! Two test families:
//!   * `check_due_*` — T-attr-2c read-side. Use `common::unique_scope()`
//!     for parallel isolation (read-only, so no scope-writer collision).
//!   * `tick_*` — T-attr-2d apply-side. Use `Scope::Global` because the
//!     `dispatch_maintenance` writer hardcodes `Scope::Global` (the only
//!     production scope by §3 design). Serialised via `DECAY_TEST_LOCK`
//!     against parallel decay tests and the four `DECAY_TARGETS` global
//!     rows are snapshot/restored around each test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use chrono::{Duration as ChronoDuration, SubsecRound, Utc};
use serde_json::json;
use tokio::sync::Mutex;

use satan_attrd::{AttributeName, DECAY_TARGETS, DecayScheduler, FakeClock, Scope};

/// Serialises tests that mutate the four `DECAY_TARGETS` rows at
/// `scope = 'global'`. Parallel `tick_*` tests would race each other's
/// `value` + `last_decay_at` reads; the read-only `check_due_*` tests at
/// unique scopes do not need this lock.
static DECAY_TEST_LOCK: Mutex<()> = Mutex::const_new(());

async fn set_last_decay_at(
    pool: &sqlx::PgPool,
    scope: &str,
    name: &str,
    last: chrono::DateTime<Utc>,
) {
    sqlx::query(
        "UPDATE satan_attributes SET last_decay_at = $1
         WHERE scope = $2 AND name = $3",
    )
    .bind(last)
    .bind(scope)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
}

async fn fetch_state(
    pool: &sqlx::PgPool,
    scope: &str,
    name: &str,
) -> (f64, Option<chrono::DateTime<Utc>>) {
    sqlx::query_as(
        "SELECT value, last_decay_at FROM satan_attributes
         WHERE scope = $1 AND name = $2",
    )
    .bind(scope)
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn check_due_returns_rows_with_null_last_decay_at() {
    let pool = common::shared_pool().await;
    let scope = common::unique_scope();

    for name in DECAY_TARGETS {
        common::upsert_raw(&pool, &scope, name.as_str(), 0.5, &json!({})).await;
        // upsert_raw leaves last_decay_at at the column default (NULL).
    }

    let clock = Arc::new(FakeClock::new(Utc::now()));
    let scheduler = DecayScheduler::new(pool.clone(), clock, scope.clone());
    let due = scheduler.check_due().await.unwrap();
    assert_eq!(
        due.len(),
        DECAY_TARGETS.len(),
        "all 4 targets at NULL last_decay_at should be due"
    );
    for row in &due {
        assert!(
            row.last_decay_at.is_none(),
            "{}: expected NULL last_decay_at",
            row.name
        );
        assert!(
            row.days_since_last.is_none(),
            "{}: expected None days_since_last",
            row.name
        );
    }

    common::cleanup_scope(&pool, &scope).await;
}

#[tokio::test]
async fn check_due_returns_rows_older_than_24h() {
    let pool = common::shared_pool().await;
    let scope = common::unique_scope();
    let now = Utc::now();
    let stale = now - ChronoDuration::hours(25);

    for name in DECAY_TARGETS {
        common::upsert_raw(&pool, &scope, name.as_str(), 0.5, &json!({})).await;
        set_last_decay_at(&pool, &scope, name.as_str(), stale).await;
    }

    let clock = Arc::new(FakeClock::new(now));
    let scheduler = DecayScheduler::new(pool.clone(), clock, scope.clone());
    let due = scheduler.check_due().await.unwrap();
    assert_eq!(due.len(), DECAY_TARGETS.len());
    for row in &due {
        assert_eq!(row.days_since_last, Some(1), "{}: 25h is 1 whole day", row.name);
    }

    common::cleanup_scope(&pool, &scope).await;
}

#[tokio::test]
async fn check_due_skips_rows_within_24h() {
    let pool = common::shared_pool().await;
    let scope = common::unique_scope();
    let now = Utc::now();
    let fresh = now - ChronoDuration::hours(23);

    for name in DECAY_TARGETS {
        common::upsert_raw(&pool, &scope, name.as_str(), 0.5, &json!({})).await;
        set_last_decay_at(&pool, &scope, name.as_str(), fresh).await;
    }

    let clock = Arc::new(FakeClock::new(now));
    let scheduler = DecayScheduler::new(pool.clone(), clock, scope.clone());
    let due = scheduler.check_due().await.unwrap();
    assert!(
        due.is_empty(),
        "23h-old rows should not be due; got {} rows",
        due.len()
    );

    common::cleanup_scope(&pool, &scope).await;
}

#[tokio::test]
async fn check_due_ignores_non_decay_target_attributes() {
    let pool = common::shared_pool().await;
    let scope = common::unique_scope();

    // Seed Curiosity (NOT a decay target) at NULL last_decay_at — would be
    // "due" by the staleness rule if the target filter were missing.
    common::upsert_raw(&pool, &scope, "curiosity", 0.5, &json!({})).await;
    // Seed one target so we know the scope is non-empty.
    common::upsert_raw(&pool, &scope, "shame", 0.5, &json!({})).await;
    sqlx::query(
        "UPDATE satan_attributes SET last_decay_at = NOW()
         WHERE scope = $1 AND name = 'shame'",
    )
    .bind(&scope)
    .execute(&pool)
    .await
    .unwrap();

    let clock = Arc::new(FakeClock::new(Utc::now()));
    let scheduler = DecayScheduler::new(pool.clone(), clock, scope.clone());
    let due = scheduler.check_due().await.unwrap();
    assert!(
        due.iter().all(|r| DECAY_TARGETS.contains(&r.name)),
        "non-target attributes must not appear in due set"
    );
    assert!(
        !due.iter().any(|r| r.name == AttributeName::Curiosity),
        "Curiosity (positive-pole) must not be a decay target"
    );

    common::cleanup_scope(&pool, &scope).await;
}

#[tokio::test]
async fn check_due_handles_mixed_freshness() {
    // Two stale, two fresh: only the stale pair appears in `due`.
    let pool = common::shared_pool().await;
    let scope = common::unique_scope();
    let now = Utc::now();
    let stale = now - ChronoDuration::hours(48);
    let fresh = now - ChronoDuration::hours(1);

    let pairs = [
        (AttributeName::Shame, stale),
        (AttributeName::Doubt, fresh),
        (AttributeName::Brooding, stale),
        (AttributeName::Metamorphosis, fresh),
    ];
    for (name, ts) in pairs {
        common::upsert_raw(&pool, &scope, name.as_str(), 0.5, &json!({})).await;
        set_last_decay_at(&pool, &scope, name.as_str(), ts).await;
    }

    let clock = Arc::new(FakeClock::new(now));
    let scheduler = DecayScheduler::new(pool.clone(), clock, scope.clone());
    let due = scheduler.check_due().await.unwrap();
    let due_names: Vec<AttributeName> = due.iter().map(|r| r.name).collect();
    assert_eq!(due.len(), 2);
    assert!(due_names.contains(&AttributeName::Shame));
    assert!(due_names.contains(&AttributeName::Brooding));
    for row in &due {
        assert_eq!(row.days_since_last, Some(2));
    }

    common::cleanup_scope(&pool, &scope).await;
}

// ---------------------------------------------------------------------------
// T-attr-2d apply-side: tick fires (golden + floor)
// ---------------------------------------------------------------------------

async fn snapshot_decay_targets_global(pool: &sqlx::PgPool) -> Vec<(String, f64, Option<chrono::DateTime<Utc>>, serde_json::Value)> {
    let names: Vec<&'static str> = DECAY_TARGETS.iter().map(|n| n.as_str()).collect();
    sqlx::query_as(
        "SELECT name, value, last_decay_at, evidence_json
         FROM satan_attributes
         WHERE scope = 'global' AND name = ANY($1)",
    )
    .bind(&names)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn restore_decay_targets_global(
    pool: &sqlx::PgPool,
    rows: &[(String, f64, Option<chrono::DateTime<Utc>>, serde_json::Value)],
) {
    for (name, value, last_decay_at, evidence_json) in rows {
        sqlx::query(
            "UPDATE satan_attributes
             SET value = $1, last_decay_at = $2, evidence_json = $3, updated_at = NOW()
             WHERE scope = 'global' AND name = $4",
        )
        .bind(value)
        .bind(*last_decay_at)
        .bind(evidence_json)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    }
}

async fn purge_test_decay_events(pool: &sqlx::PgPool, run_id: &str) {
    sqlx::query("DELETE FROM satan_attribute_events WHERE run_id = $1")
        .bind(run_id)
        .execute(pool)
        .await
        .unwrap();
    let prefix = format!("{run_id}.%");
    sqlx::query("DELETE FROM satan_audit_inbox WHERE payload_json->>'id' LIKE $1")
        .bind(&prefix)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn tick_applies_decay_and_bumps_last_decay_at() {
    let _lock = DECAY_TEST_LOCK.lock().await;
    let pool = common::shared_pool().await;
    let snapshot = snapshot_decay_targets_global(&pool).await;

    // Truncate to microseconds — PG TIMESTAMPTZ stores us-precision; chrono
    // ns-precision values round-trip lossy. Truncating up front keeps the
    // post-fetch equality clean.
    let now = Utc::now().trunc_subsecs(6);
    let stale = now - ChronoDuration::hours(48);
    let expected_run_id_pre = format!("maintenance:{}", now.date_naive().format("%Y-%m-%d"));
    // Pre-purge: a prior failed run can leave attribute_events at this
    // run_id, breaking the seq=1 uniqueness guarantee on re-run.
    purge_test_decay_events(&pool, &expected_run_id_pre).await;

    // Force each global decay-target row to a known (0.50, stale) state.
    for name in DECAY_TARGETS {
        sqlx::query(
            "UPDATE satan_attributes
             SET value = 0.50, last_decay_at = $1, evidence_json = '{}'::jsonb
             WHERE scope = 'global' AND name = $2",
        )
        .bind(stale)
        .bind(name.as_str())
        .execute(&pool)
        .await
        .unwrap();
    }

    let scope = Scope::Global.as_str().to_string();
    let clock = Arc::new(FakeClock::new(now));
    let scheduler = DecayScheduler::new(pool.clone(), clock, scope);
    let count = scheduler.tick().await.unwrap();
    assert_eq!(count, DECAY_TARGETS.len(), "all 4 targets should fire");

    let expected_run_id = format!("maintenance:{}", now.date_naive().format("%Y-%m-%d"));

    for name in DECAY_TARGETS {
        let (value, last) = fetch_state(&pool, "global", name.as_str()).await;
        assert!(
            (value - 0.49).abs() < 1e-9,
            "{name}: expected 0.49 after -0.01 decay, got {value}"
        );
        assert_eq!(
            last,
            Some(now),
            "{name}: last_decay_at should be bumped to tick `now`"
        );

        let event: (String, String, f64, f64, serde_json::Value, bool) = sqlx::query_as(
            "SELECT source, reason, old_value, new_value, evidence_json, disabled
             FROM satan_attribute_events
             WHERE run_id = $1 AND scope = 'global' AND name = $2",
        )
        .bind(&expected_run_id)
        .bind(name.as_str())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(event.0, "maintenance");
        assert_eq!(event.1, "idle_decay");
        assert!((event.2 - 0.50).abs() < 1e-9);
        assert!((event.3 - 0.49).abs() < 1e-9);
        assert_eq!(event.4["days_since_last"].as_i64(), Some(2));
        assert_eq!(
            event.4["tick_utc_day"].as_str(),
            Some(now.date_naive().format("%Y-%m-%d").to_string().as_str())
        );
        assert!(!event.5, "{name}: event should not be marked disabled");
    }

    // Audit inbox got 4 rows for this run (one per attribute event).
    let audit_prefix = format!("{expected_run_id}.%");
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM satan_audit_inbox WHERE payload_json->>'id' LIKE $1",
    )
    .bind(&audit_prefix)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audit_count, DECAY_TARGETS.len() as i64);

    purge_test_decay_events(&pool, &expected_run_id).await;
    restore_decay_targets_global(&pool, &snapshot).await;
}

#[tokio::test]
async fn tick_clamps_floor_to_zero_with_range_clamp_cap() {
    let _lock = DECAY_TEST_LOCK.lock().await;
    let pool = common::shared_pool().await;
    let snapshot = snapshot_decay_targets_global(&pool).await;

    let now = Utc::now().trunc_subsecs(6);
    let stale = now - ChronoDuration::hours(48);
    let run_id_pre = format!("maintenance:{}", now.date_naive().format("%Y-%m-%d"));
    purge_test_decay_events(&pool, &run_id_pre).await;

    // Seed Shame at 0.005 — one -0.01 tick clamps to 0.0 with range_clamp.
    // Other targets parked at a fresh last_decay_at so they don't fire and
    // pollute the assertion set.
    sqlx::query(
        "UPDATE satan_attributes
         SET value = 0.005, last_decay_at = $1, evidence_json = '{}'::jsonb
         WHERE scope = 'global' AND name = 'shame'",
    )
    .bind(stale)
    .execute(&pool)
    .await
    .unwrap();
    for name in [
        AttributeName::Doubt,
        AttributeName::Brooding,
        AttributeName::Metamorphosis,
    ] {
        sqlx::query(
            "UPDATE satan_attributes
             SET value = 0.50, last_decay_at = NOW(), evidence_json = '{}'::jsonb
             WHERE scope = 'global' AND name = $1",
        )
        .bind(name.as_str())
        .execute(&pool)
        .await
        .unwrap();
    }

    let clock = Arc::new(FakeClock::new(now));
    let scheduler = DecayScheduler::new(
        pool.clone(),
        clock,
        Scope::Global.as_str().to_string(),
    );
    let count = scheduler.tick().await.unwrap();
    assert_eq!(count, 1, "only Shame should be due");

    let (value, last) = fetch_state(&pool, "global", "shame").await;
    assert!(value.abs() < 1e-9, "expected floor at 0.0, got {value}");
    assert_eq!(last, Some(now), "last_decay_at should be bumped");

    let run_id = format!("maintenance:{}", now.date_naive().format("%Y-%m-%d"));
    let caps_json: serde_json::Value = sqlx::query_scalar(
        "SELECT caps_applied FROM satan_attribute_events
         WHERE run_id = $1 AND scope = 'global' AND name = 'shame'",
    )
    .bind(&run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(caps_json, json!(["range_clamp"]));

    purge_test_decay_events(&pool, &run_id).await;
    restore_decay_targets_global(&pool, &snapshot).await;
}
