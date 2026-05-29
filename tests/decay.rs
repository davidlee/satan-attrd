//! Decay scheduler integration tests (T-attr-2c — Clock seam +
//! check_due + tick read-side).
//!
//! T-attr-2c is the **skeleton**: the scheduler identifies due rows and
//! logs, but does NOT mutate state. T-attr-2d will extend `tick` to
//! dispatch synthetic `(maintenance, idle_decay)` events; the
//! `tick_does_not_mutate_state` test enforces the skeleton boundary.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use chrono::{Duration as ChronoDuration, SubsecRound, Utc};
use serde_json::json;

use satan_attrd::{AttributeName, DECAY_TARGETS, DecayScheduler, FakeClock};

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

#[tokio::test]
async fn tick_does_not_mutate_state() {
    // T-attr-2c skeleton boundary: tick reads + logs, never writes.
    // T-attr-2d will lift this guard when firing lands.
    let pool = common::shared_pool().await;
    let scope = common::unique_scope();
    // Truncate to microseconds — PG TIMESTAMPTZ stores us-precision, so a
    // chrono ns-precision value round-trips lossy. Truncating up-front
    // keeps the post-fetch equality clean without a tolerance window.
    let now = Utc::now().trunc_subsecs(6);
    let stale = now - ChronoDuration::hours(48);

    for name in DECAY_TARGETS {
        common::upsert_raw(&pool, &scope, name.as_str(), 0.5, &json!({})).await;
        set_last_decay_at(&pool, &scope, name.as_str(), stale).await;
    }

    let clock = Arc::new(FakeClock::new(now));
    let scheduler = DecayScheduler::new(pool.clone(), clock, scope.clone());
    let count = scheduler.tick().await.unwrap();
    assert_eq!(count, DECAY_TARGETS.len());

    for name in DECAY_TARGETS {
        let (value, last) = fetch_state(&pool, &scope, name.as_str()).await;
        assert!(
            (value - 0.5).abs() < 1e-9,
            "{}: value mutated by tick (skeleton must not write)",
            name
        );
        assert_eq!(
            last,
            Some(stale),
            "{}: last_decay_at mutated by tick (skeleton must not bump)",
            name
        );
    }

    // No event rows written either — `satan_attribute_events` for this
    // scope should remain empty since no source events were dispatched.
    let event_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM satan_attribute_events WHERE scope = $1",
    )
    .bind(&scope)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        event_count, 0,
        "tick skeleton must not emit attribute events"
    );

    common::cleanup_scope(&pool, &scope).await;
}
