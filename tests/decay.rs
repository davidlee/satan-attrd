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

use satan_attrd::{AttributeName, DECAY_TARGETS, DecayScheduler, FakeClock, Scope, store};

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

/// Force a global decay-target row to a known `(value, last_decay_at)`.
/// `last = None` writes SQL NULL (the "decay never ran" state).
async fn force_target(
    pool: &sqlx::PgPool,
    name: &str,
    value: f64,
    last: Option<chrono::DateTime<Utc>>,
) {
    sqlx::query(
        "UPDATE satan_attributes
         SET value = $1, last_decay_at = $2, evidence_json = '{}'::jsonb
         WHERE scope = 'global' AND name = $3",
    )
    .bind(value)
    .bind(last)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
}

/// Pin the decay disable-switch to enabled at the start of a tick test, so a
/// predecessor that panicked before resetting it cannot leak `false` into a
/// test that assumes the enabled path. (The setting is shared DB state; the
/// `DECAY_TEST_LOCK` serialises tick tests but does not roll back on panic.)
async fn enable_attribute_updates(pool: &sqlx::PgPool) {
    store::set_setting_bool(pool, "attribute_updates_enabled", true)
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
        assert_eq!(
            row.days_since_last,
            Some(1),
            "{}: 25h is 1 whole day",
            row.name
        );
    }
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
}

// ---------------------------------------------------------------------------
// T-attr-2d apply-side: tick fires (golden + floor)
// ---------------------------------------------------------------------------

async fn snapshot_decay_targets_global(
    pool: &sqlx::PgPool,
) -> Vec<(
    String,
    f64,
    Option<chrono::DateTime<Utc>>,
    serde_json::Value,
)> {
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
    rows: &[(
        String,
        f64,
        Option<chrono::DateTime<Utc>>,
        serde_json::Value,
    )],
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

/// Highest persisted `seq` for a run_id (`None` when no events) — mirrors
/// `store::max_seq_for_run` so tests assert the counter-resume invariant
/// against the DB directly.
async fn max_seq(pool: &sqlx::PgPool, run_id: &str) -> Option<i32> {
    sqlx::query_scalar("SELECT MAX(seq) FROM satan_attribute_events WHERE run_id = $1")
        .bind(run_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn tick_applies_decay_and_bumps_last_decay_at() {
    let _lock = DECAY_TEST_LOCK.lock().await;
    let pool = common::shared_pool().await;
    let snapshot = snapshot_decay_targets_global(&pool).await;
    enable_attribute_updates(&pool).await;

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
    enable_attribute_updates(&pool).await;

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
    let scheduler = DecayScheduler::new(pool.clone(), clock, Scope::Global.as_str().to_string());
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

// ---------------------------------------------------------------------------
// T-attr-2e integration matrix: catch-up, disable, restart, replay-determinism
// ---------------------------------------------------------------------------

/// §8 / handover:149–154 — a multi-day gap is collapsed into ONE event with a
/// single -0.01 delta; the gap is preserved in `evidence_json.days_since_last`
/// for observability, NOT multiplied into the delta.
#[tokio::test]
async fn tick_catch_up_emits_single_event_for_multi_day_gap() {
    let _lock = DECAY_TEST_LOCK.lock().await;
    let pool = common::shared_pool().await;
    let snapshot = snapshot_decay_targets_global(&pool).await;
    enable_attribute_updates(&pool).await;

    let now = Utc::now().trunc_subsecs(6);
    let stale = now - ChronoDuration::days(5);
    let run_id = format!("maintenance:{}", now.date_naive().format("%Y-%m-%d"));
    purge_test_decay_events(&pool, &run_id).await;

    // Only Shame is stale; the others are fresh so the assertion set is just Shame.
    force_target(&pool, "shame", 0.50, Some(stale)).await;
    for name in [
        AttributeName::Doubt,
        AttributeName::Brooding,
        AttributeName::Metamorphosis,
    ] {
        force_target(&pool, name.as_str(), 0.50, Some(now)).await;
    }

    let clock = Arc::new(FakeClock::new(now));
    let scheduler = DecayScheduler::new(pool.clone(), clock, Scope::Global.as_str().to_string());
    let count = scheduler.tick().await.unwrap();
    assert_eq!(count, 1, "only the 5-day-stale target should fire");

    let rows: Vec<(f64, f64, serde_json::Value)> = sqlx::query_as(
        "SELECT old_value, new_value, evidence_json FROM satan_attribute_events
         WHERE run_id = $1 AND scope = 'global' AND name = 'shame'",
    )
    .bind(&run_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "catch-up collapses a multi-day gap into one event"
    );
    let (old, new, evidence) = &rows[0];
    assert!((old - 0.50).abs() < 1e-9);
    assert!(
        (new - 0.49).abs() < 1e-9,
        "single -0.01 delta, not multiplied by the 5-day gap, got {new}"
    );
    assert_eq!(
        evidence["days_since_last"].as_i64(),
        Some(5),
        "the gap is preserved in evidence even though the delta is single"
    );

    let (value, _) = fetch_state(&pool, "global", "shame").await;
    assert!((value - 0.49).abs() < 1e-9);

    purge_test_decay_events(&pool, &run_id).await;
    restore_decay_targets_global(&pool, &snapshot).await;
}

/// §17.5 / §17.8 — when `attribute_updates_enabled` is false a decay tick still
/// inserts the event (`disabled = true`) and enqueues audit, but does NOT upsert
/// the projection and does NOT bump `last_decay_at`. The non-bump is load-bearing:
/// on re-enable the row is still due, so decay resumes on the next tick.
#[tokio::test]
async fn tick_disabled_inserts_event_and_audit_but_skips_projection() {
    let _lock = DECAY_TEST_LOCK.lock().await;
    let pool = common::shared_pool().await;
    let snapshot = snapshot_decay_targets_global(&pool).await;
    enable_attribute_updates(&pool).await;

    let now = Utc::now().trunc_subsecs(6);
    let stale = now - ChronoDuration::hours(48);
    let run_id = format!("maintenance:{}", now.date_naive().format("%Y-%m-%d"));
    purge_test_decay_events(&pool, &run_id).await;

    store::set_setting_bool(&pool, "attribute_updates_enabled", false)
        .await
        .unwrap();

    // Only Shame due.
    force_target(&pool, "shame", 0.50, Some(stale)).await;
    for name in [
        AttributeName::Doubt,
        AttributeName::Brooding,
        AttributeName::Metamorphosis,
    ] {
        force_target(&pool, name.as_str(), 0.50, Some(now)).await;
    }

    let clock = Arc::new(FakeClock::new(now));
    let scheduler = DecayScheduler::new(pool.clone(), clock, Scope::Global.as_str().to_string());
    let count = scheduler.tick().await.unwrap();
    assert_eq!(count, 1);

    // Event row inserted, flagged disabled.
    let (disabled,): (bool,) = sqlx::query_as(
        "SELECT disabled FROM satan_attribute_events
         WHERE run_id = $1 AND scope = 'global' AND name = 'shame'",
    )
    .bind(&run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(disabled, "disabled tick must mark the event disabled");

    // Audit enqueued regardless of the disable switch.
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM satan_audit_inbox WHERE payload_json->>'id' LIKE $1",
    )
    .bind(format!("{run_id}.%"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audit_count, 1, "audit must be enqueued even when disabled");

    // Projection untouched: value unchanged, last_decay_at NOT bumped.
    let (value, last) = fetch_state(&pool, "global", "shame").await;
    assert!(
        (value - 0.50).abs() < 1e-9,
        "disabled tick must not upsert the projection value, got {value}"
    );
    assert_eq!(
        last,
        Some(stale),
        "disabled tick must NOT bump last_decay_at (§17.8)"
    );

    // Re-enable → next tick fires, proving the non-bump kept the row due.
    store::set_setting_bool(&pool, "attribute_updates_enabled", true)
        .await
        .unwrap();
    let count2 = scheduler.tick().await.unwrap();
    assert_eq!(
        count2, 1,
        "re-enabled tick must fire because last_decay_at was never bumped"
    );
    let (value2, last2) = fetch_state(&pool, "global", "shame").await;
    assert!(
        (value2 - 0.49).abs() < 1e-9,
        "re-enabled tick applies decay"
    );
    assert_eq!(last2, Some(now), "re-enabled tick bumps last_decay_at");

    // Teardown: restore the setting before releasing the lock.
    store::set_setting_bool(&pool, "attribute_updates_enabled", true)
        .await
        .unwrap();
    purge_test_decay_events(&pool, &run_id).await;
    restore_decay_targets_global(&pool, &snapshot).await;
}

/// Restart durability — scheduler state lives in the DB (`last_decay_at`), not
/// the in-memory per-day counter. A fresh scheduler the same UTC day is a no-op
/// (rows already fresh); the next UTC day re-fires under a new `run_id` where a
/// counter-from-zero is safe.
#[tokio::test]
async fn tick_survives_scheduler_restart_via_last_decay_at() {
    let _lock = DECAY_TEST_LOCK.lock().await;
    let pool = common::shared_pool().await;
    let snapshot = snapshot_decay_targets_global(&pool).await;
    enable_attribute_updates(&pool).await;

    let day0 = Utc::now().trunc_subsecs(6);
    let stale = day0 - ChronoDuration::hours(48);
    let day1 = day0 + ChronoDuration::hours(25); // strictly >24h and a later UTC date
    let run_id0 = format!("maintenance:{}", day0.date_naive().format("%Y-%m-%d"));
    let run_id1 = format!("maintenance:{}", day1.date_naive().format("%Y-%m-%d"));
    purge_test_decay_events(&pool, &run_id0).await;
    purge_test_decay_events(&pool, &run_id1).await;

    for name in DECAY_TARGETS {
        force_target(&pool, name.as_str(), 0.50, Some(stale)).await;
    }

    let clock = Arc::new(FakeClock::new(day0));
    // Scheduler #1 fires and bumps last_decay_at to day0.
    {
        let s1 = DecayScheduler::new(
            pool.clone(),
            clock.clone(),
            Scope::Global.as_str().to_string(),
        );
        assert_eq!(s1.tick().await.unwrap(), DECAY_TARGETS.len());
    } // s1 dropped — its in-memory counter is gone.

    // Scheduler #2: restart, same day. State lives in the DB so nothing is due.
    let s2 = DecayScheduler::new(
        pool.clone(),
        clock.clone(),
        Scope::Global.as_str().to_string(),
    );
    assert_eq!(
        s2.tick().await.unwrap(),
        0,
        "post-restart same-day tick must be a no-op (rows fresh in DB)"
    );

    // Next UTC day, fresh scheduler → re-fires under a new run_id.
    clock.set(day1);
    let s3 = DecayScheduler::new(
        pool.clone(),
        clock.clone(),
        Scope::Global.as_str().to_string(),
    );
    assert_eq!(
        s3.tick().await.unwrap(),
        DECAY_TARGETS.len(),
        "next UTC day re-fires after restart"
    );

    purge_test_decay_events(&pool, &run_id0).await;
    purge_test_decay_events(&pool, &run_id1).await;
    restore_decay_targets_global(&pool, &snapshot).await;
}

/// §10.5 / §17.8 — `rebuild_projection` replays the event log from zero and
/// resets `last_decay_at` to NULL, which re-arms decay (a rebuilt projection is
/// treated as "decay never ran"). Replay is deterministic: a second rebuild
/// reproduces identical values. (Generic disabled-skip / replay-all behaviour is
/// covered in tests/store.rs; this test asserts only the decay-specific clock
/// reset + re-arm.)
#[tokio::test]
async fn rebuild_clears_last_decay_at_so_decay_rearms() {
    let _lock = DECAY_TEST_LOCK.lock().await;
    let pool = common::shared_pool().await;
    let snapshot = snapshot_decay_targets_global(&pool).await;
    enable_attribute_updates(&pool).await;

    let now = Utc::now().trunc_subsecs(6);
    let stale = now - ChronoDuration::hours(48);
    let run_id = format!("maintenance:{}", now.date_naive().format("%Y-%m-%d"));
    purge_test_decay_events(&pool, &run_id).await;

    for name in DECAY_TARGETS {
        force_target(&pool, name.as_str(), 0.50, Some(stale)).await;
    }

    // Enabled tick bumps last_decay_at to `now` for every target.
    let clock = Arc::new(FakeClock::new(now));
    let scheduler = DecayScheduler::new(pool.clone(), clock, Scope::Global.as_str().to_string());
    assert_eq!(scheduler.tick().await.unwrap(), DECAY_TARGETS.len());
    for name in DECAY_TARGETS {
        let (_, last) = fetch_state(&pool, "global", name.as_str()).await;
        assert_eq!(last, Some(now), "{name}: tick should bump last_decay_at");
    }

    // Rebuild from the event log: zeros + replays, resetting last_decay_at to NULL.
    store::rebuild_projection(&pool, false).await.unwrap();
    let mut values_after_first_rebuild = Vec::new();
    for name in DECAY_TARGETS {
        let (value, last) = fetch_state(&pool, "global", name.as_str()).await;
        assert_eq!(last, None, "{name}: rebuild must clear last_decay_at");
        values_after_first_rebuild.push(value);
    }

    // Re-arm: every target is due again because last_decay_at is NULL.
    let due = scheduler.check_due().await.unwrap();
    assert_eq!(
        due.len(),
        DECAY_TARGETS.len(),
        "every target must be due again after rebuild clears last_decay_at"
    );

    // Determinism: a second rebuild reproduces identical values.
    store::rebuild_projection(&pool, false).await.unwrap();
    for (name, v0) in DECAY_TARGETS.iter().zip(values_after_first_rebuild) {
        let (v1, _) = fetch_state(&pool, "global", name.as_str()).await;
        assert!(
            (v1 - v0).abs() < 1e-9,
            "{name}: rebuild must be deterministic ({v0} vs {v1})"
        );
    }

    purge_test_decay_events(&pool, &run_id).await;
    restore_decay_targets_global(&pool, &snapshot).await;
}

/// T-attr-2f — restart while disabled, same UTC day, resumes cleanly. Disabled
/// ticks never bump `last_decay_at`, so cold (NULL) targets stay due across a
/// restart. The per-day counter is resumed from the persisted `MAX(seq)+1` on
/// the first post-restart tick, so the second scheduler emits a fresh seq range
/// rather than re-emitting `1..=N` and colliding with `UNIQUE (run_id, seq)`.
/// (Flips the T-attr-2e probe that asserted the pre-fix loud `DecaySeqCollision`.)
#[tokio::test]
async fn tick_restart_while_disabled_same_day_resumes_cleanly() {
    let _lock = DECAY_TEST_LOCK.lock().await;
    let pool = common::shared_pool().await;
    let snapshot = snapshot_decay_targets_global(&pool).await;
    enable_attribute_updates(&pool).await;

    let now = Utc::now().trunc_subsecs(6);
    let run_id = format!("maintenance:{}", now.date_naive().format("%Y-%m-%d"));
    purge_test_decay_events(&pool, &run_id).await;

    store::set_setting_bool(&pool, "attribute_updates_enabled", false)
        .await
        .unwrap();

    // Cold targets: NULL last_decay_at → always due; disabled ticks never bump.
    for name in DECAY_TARGETS {
        force_target(&pool, name.as_str(), 0.50, None).await;
    }

    let n = DECAY_TARGETS.len() as i32;
    let clock = Arc::new(FakeClock::new(now));
    // Scheduler #1 inserts seq 1..=N under today's run_id, no bump.
    {
        let s1 = DecayScheduler::new(
            pool.clone(),
            clock.clone(),
            Scope::Global.as_str().to_string(),
        );
        assert_eq!(s1.tick().await.unwrap(), DECAY_TARGETS.len());
    } // counter dies with s1.
    assert_eq!(max_seq(&pool, &run_id).await, Some(n), "s1 emits seq 1..=N");

    // Scheduler #2: restart same day. Counter resumes from MAX(seq)+1, so the
    // re-due cold targets get a fresh seq range — no collision.
    let s2 = DecayScheduler::new(
        pool.clone(),
        clock.clone(),
        Scope::Global.as_str().to_string(),
    );
    assert_eq!(
        s2.tick().await.unwrap(),
        DECAY_TARGETS.len(),
        "restart-while-disabled must resume cleanly, not collide"
    );

    // 2N distinct events under the run_id, seqs 1..=2N — every row unique.
    assert_eq!(
        max_seq(&pool, &run_id).await,
        Some(2 * n),
        "s2 resumes past s1's seqs"
    );
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM satan_attribute_events WHERE run_id = $1")
            .bind(&run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let distinct: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT seq) FROM satan_attribute_events WHERE run_id = $1",
    )
    .bind(&run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        total,
        2 * i64::from(n),
        "both ticks persisted; nothing dropped"
    );
    assert_eq!(
        distinct, total,
        "every (run_id, seq) is unique after resume"
    );

    // Teardown.
    store::set_setting_bool(&pool, "attribute_updates_enabled", true)
        .await
        .unwrap();
    purge_test_decay_events(&pool, &run_id).await;
    restore_decay_targets_global(&pool, &snapshot).await;
}
