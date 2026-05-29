//! Integration tests for the store + migration + rebuild driver.
//!
//! Requires Postgres reachable at `$DATABASE_URL`. The harness migrates on
//! first connect; subsequent tests share the pool.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use chrono::{Duration, Utc};
use serde_json::json;
use tokio::sync::Mutex;

use satan_attrd::{
    AttributeName, Cap, Counter, EventInsert, Scope, format_event_id, insert_event,
    lookup_attribute, lookup_prior_events_by_intervention, outcome_evidence_json,
    rebuild_projection, upsert_attribute,
};

// `rebuild_projection` zeros every `satan_attributes` row at the start of
// its transaction (contract §10.5 from-zero replay). Parallel rebuild tests
// within this binary would race each other's seeded projections, so this
// mutex serializes the rebuild tests. (Cross-binary races are out of scope
// — operator-triggered rebuild is single-process by construction.)
static REBUILD_LOCK: Mutex<()> = Mutex::const_new(());

// ---------------------------------------------------------------------------
// Migration + seed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn migration_seeds_eight_global_attributes() {
    let pool = common::shared_pool().await;

    // The seed places all 8 at `(scope='global', value=0.0)` at migration
    // time. Other tests share the global rows and may have updated them
    // since — assert presence + name parses, not the current value.
    for name in AttributeName::ALL {
        let row = lookup_attribute(&pool, Scope::Global, name)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("seed missing for {name}"));
        assert_eq!(row.scope, "global");
        assert_eq!(row.name, name.as_str());
        assert!(
            (0.0..=1.0).contains(&row.value),
            "seed value for {name} out of range: {}",
            row.value
        );
    }
}

#[tokio::test]
async fn migration_is_idempotent() {
    // shared_pool runs migrate; calling again should be a no-op.
    let pool = common::shared_pool().await;
    satan_attrd::migrate::run_migrations(&pool).await.unwrap();
    satan_attrd::migrate::run_migrations(&pool).await.unwrap();
}

// ---------------------------------------------------------------------------
// UPSERT round-trip (projection)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upsert_round_trip_isolated_scope() {
    let pool = common::shared_pool().await;
    let scope = common::unique_scope();

    common::upsert_raw(&pool, &scope, "shame", 0.0, &json!({})).await;
    common::upsert_raw(
        &pool,
        &scope,
        "shame",
        0.25,
        &json!({"intervention_id": "iv1"}),
    )
    .await;

    let (value, evidence) = common::select_raw(&pool, &scope, "shame").await.unwrap();
    assert!((value - 0.25).abs() < 1e-9);
    assert_eq!(evidence["intervention_id"], "iv1");

    common::cleanup_scope(&pool, &scope).await;
}

#[tokio::test]
async fn upsert_rejects_out_of_range() {
    let pool = common::shared_pool().await;
    let err = upsert_attribute(
        &pool,
        Scope::Global,
        AttributeName::Shame,
        1.5,
        &json!({}),
        Utc::now(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        satan_attrd::Error::ValueOutOfRange(v) if (v - 1.5).abs() < 1e-9
    ));
}

// ---------------------------------------------------------------------------
// Event INSERT round-trip + counter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn event_insert_round_trip() {
    let pool = common::shared_pool().await;
    let run_id = common::unique_run_id();
    let counter = Counter::new();
    let ts = Utc::now();

    let ev = EventInsert {
        run_id: run_id.clone(),
        seq: counter.next(),
        ts,
        scope: Scope::Global,
        name: AttributeName::Shame,
        old_value: 0.10,
        new_value: 0.25,
        source: "outcome".into(),
        reason: "contradicted".into(),
        evidence_json: outcome_evidence_json(
            &format!("{run_id}.iv001"),
            "contradicted",
            "medium",
            Some("notify"),
            None,
            &["focus.sway:firefox"],
            &[],
        ),
        caps_applied: vec![],
        disabled: false,
    };
    let id = insert_event(&pool, &ev).await.unwrap();
    assert_eq!(id, format_event_id(&run_id, 1));

    let prior = lookup_prior_events_by_intervention(
        &pool,
        &format!("{run_id}.iv001"),
        AttributeName::Shame,
    )
    .await
    .unwrap();
    assert_eq!(prior.len(), 1);
    let row = &prior[0];
    assert_eq!(row.id, id);
    assert_eq!(row.run_id, run_id);
    assert_eq!(row.seq, 1);
    assert_eq!(row.scope, "global");
    assert_eq!(row.name, "shame");
    assert!((row.old_value - 0.10).abs() < 1e-9);
    assert!((row.new_value - 0.25).abs() < 1e-9);
    assert!((row.delta - 0.15).abs() < 1e-9);
    assert_eq!(row.source, "outcome");
    assert_eq!(row.reason, "contradicted");
    assert_eq!(row.evidence_json["confidence"], "medium");
    assert!(row.caps_applied.as_array().unwrap().is_empty());
    assert!(!row.disabled);

    common::cleanup_run(&pool, &run_id).await;
}

#[tokio::test]
async fn counter_monotonic_within_run_with_unique_seqs_in_db() {
    let pool = common::shared_pool().await;
    let run_id = common::unique_run_id();
    let counter = Counter::new();

    let base_ts = Utc::now();
    for i in 0..5 {
        let ev = EventInsert {
            run_id: run_id.clone(),
            seq: counter.next(),
            ts: base_ts + Duration::milliseconds(i),
            scope: Scope::Global,
            name: AttributeName::Doubt,
            old_value: 0.0,
            new_value: 0.05,
            source: "outcome".into(),
            reason: "ignored".into(),
            evidence_json: outcome_evidence_json(
                &format!("{run_id}.iv00{}", i + 1),
                "ignored",
                "low",
                Some("notify"),
                None,
                &[],
                &[],
            ),
            caps_applied: vec![],
            disabled: false,
        };
        insert_event(&pool, &ev).await.unwrap();
    }
    assert_eq!(counter.peek(), 5);

    // Re-inserting at the same (run_id, seq) collides on UNIQUE.
    let collide = EventInsert {
        run_id: run_id.clone(),
        seq: 1,
        ts: base_ts,
        scope: Scope::Global,
        name: AttributeName::Hunger,
        old_value: 0.0,
        new_value: 0.0,
        source: "outcome".into(),
        reason: "neutral".into(),
        evidence_json: json!({"intervention_id": "ivX", "classification": "neutral", "confidence": "low"}),
        caps_applied: vec![],
        disabled: false,
    };
    let err = insert_event(&pool, &collide).await.unwrap_err();
    assert!(
        matches!(err, satan_attrd::Error::Sqlx(_)),
        "expected UNIQUE collision, got {err:?}"
    );

    common::cleanup_run(&pool, &run_id).await;
}

#[tokio::test]
async fn caps_applied_round_trip() {
    let pool = common::shared_pool().await;
    let run_id = common::unique_run_id();

    let ev = EventInsert {
        run_id: run_id.clone(),
        seq: 1,
        ts: Utc::now(),
        scope: Scope::Global,
        name: AttributeName::Friction,
        old_value: 0.20,
        new_value: 0.30,
        source: "outcome".into(),
        reason: "harmful".into(),
        evidence_json: outcome_evidence_json(
            &format!("{run_id}.iv001"),
            "harmful",
            "high",
            Some("notify"),
            None,
            &[],
            &[],
        ),
        caps_applied: vec![Cap::FrictionCap, Cap::RangeClamp],
        disabled: false,
    };
    insert_event(&pool, &ev).await.unwrap();

    let prior = lookup_prior_events_by_intervention(
        &pool,
        &format!("{run_id}.iv001"),
        AttributeName::Friction,
    )
    .await
    .unwrap();
    assert_eq!(prior.len(), 1);
    let caps = prior[0].caps_applied.as_array().unwrap();
    assert_eq!(caps.len(), 2);
    assert_eq!(caps[0], "friction_cap");
    assert_eq!(caps[1], "range_clamp");

    common::cleanup_run(&pool, &run_id).await;
}

// ---------------------------------------------------------------------------
// Prior-event lookup uses the expression index
// ---------------------------------------------------------------------------

#[tokio::test]
async fn prior_event_lookup_filters_by_intervention_and_name() {
    let pool = common::shared_pool().await;
    let run_id = common::unique_run_id();
    let iv_a = format!("{run_id}.iv001");
    let iv_b = format!("{run_id}.iv002");

    let counter = Counter::new();
    let base_ts = Utc::now();
    // iv_a writes to shame + doubt; iv_b writes to shame only.
    for (i, (iv, name, val)) in [
        (&iv_a, AttributeName::Shame, 0.15),
        (&iv_a, AttributeName::Doubt, 0.15),
        (&iv_b, AttributeName::Shame, 0.30),
    ]
    .into_iter()
    .enumerate()
    {
        let ev = EventInsert {
            run_id: run_id.clone(),
            seq: counter.next(),
            ts: base_ts + Duration::milliseconds(i as i64),
            scope: Scope::Global,
            name,
            old_value: 0.0,
            new_value: val,
            source: "outcome".into(),
            reason: "harmful".into(),
            evidence_json: outcome_evidence_json(
                iv,
                "harmful",
                "medium",
                Some("notify"),
                None,
                &[],
                &[],
            ),
            caps_applied: vec![],
            disabled: false,
        };
        insert_event(&pool, &ev).await.unwrap();
    }

    let shame_for_a = lookup_prior_events_by_intervention(&pool, &iv_a, AttributeName::Shame)
        .await
        .unwrap();
    assert_eq!(shame_for_a.len(), 1);
    assert_eq!(shame_for_a[0].name, "shame");

    let doubt_for_a = lookup_prior_events_by_intervention(&pool, &iv_a, AttributeName::Doubt)
        .await
        .unwrap();
    assert_eq!(doubt_for_a.len(), 1);

    let shame_for_b = lookup_prior_events_by_intervention(&pool, &iv_b, AttributeName::Shame)
        .await
        .unwrap();
    assert_eq!(shame_for_b.len(), 1);
    assert!((shame_for_b[0].new_value - 0.30).abs() < 1e-9);

    common::cleanup_run(&pool, &run_id).await;
}

#[tokio::test]
async fn prior_event_lookup_uses_expression_index() {
    let pool = common::shared_pool().await;
    let run_id = common::unique_run_id();
    let counter = Counter::new();
    let base_ts = Utc::now();

    // Bulk-seed + ANALYZE. With only a handful of rows the planner picks a
    // Seq Scan regardless of indexing — index cost > seq-scan cost on a tiny
    // relation. Seed 500 events with distinct intervention_ids so a lookup
    // by one id has ~0.2% selectivity, then ANALYZE so the planner sees the
    // distribution and prefers the expression index.
    for i in 0..500 {
        let ev = EventInsert {
            run_id: run_id.clone(),
            seq: counter.next(),
            ts: base_ts + Duration::milliseconds(i),
            scope: Scope::Global,
            name: AttributeName::Shame,
            old_value: 0.0,
            new_value: 0.05,
            source: "outcome".into(),
            reason: "ignored".into(),
            evidence_json: outcome_evidence_json(
                &format!("{run_id}.iv{i:04}"),
                "ignored",
                "low",
                Some("notify"),
                None,
                &[],
                &[],
            ),
            caps_applied: vec![],
            disabled: false,
        };
        insert_event(&pool, &ev).await.unwrap();
    }
    sqlx::query("ANALYZE satan_attribute_events")
        .execute(&pool)
        .await
        .unwrap();

    let target_iv = format!("{run_id}.iv0042");
    let plan: Vec<(String,)> = sqlx::query_as(
        "EXPLAIN SELECT id FROM satan_attribute_events
         WHERE evidence_json->>'intervention_id' = $1 AND name = $2",
    )
    .bind(&target_iv)
    .bind("shame")
    .fetch_all(&pool)
    .await
    .unwrap();
    let plan_text = plan
        .iter()
        .map(|(s,)| s.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        plan_text.contains("satan_attribute_events_iv_idx"),
        "expression index should appear in EXPLAIN plan; got:\n{plan_text}"
    );

    common::cleanup_run(&pool, &run_id).await;
}

// ---------------------------------------------------------------------------
// Rebuild from event log
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rebuild_replays_events_in_ts_run_seq_order() {
    let _lock = REBUILD_LOCK.lock().await;
    let pool = common::shared_pool().await;
    let run_id = common::unique_run_id();
    let scope = common::unique_scope();
    let counter = Counter::new();

    // Two events to the same (scope, shame). The second must win after replay.
    let base = Utc::now();
    let ev1 = sqlx::query(
        "INSERT INTO satan_attribute_events
           (id, ts, run_id, seq, scope, name, old_value, new_value, delta,
            source, reason, evidence_json, caps_applied, disabled)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    );
    ev1.bind(format!("{run_id}.attr001"))
        .bind(base)
        .bind(&run_id)
        .bind(counter.next())
        .bind(&scope)
        .bind("shame")
        .bind(0.0_f64)
        .bind(0.10_f64)
        .bind(0.10_f64)
        .bind("outcome")
        .bind("ignored")
        .bind(sqlx::types::Json(
            json!({"intervention_id": format!("{run_id}.iv001"),
                                       "classification": "ignored",
                                       "confidence": "medium"}),
        ))
        .bind(sqlx::types::Json(json!([])))
        .bind(false)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO satan_attribute_events
           (id, ts, run_id, seq, scope, name, old_value, new_value, delta,
            source, reason, evidence_json, caps_applied, disabled)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(format!("{run_id}.attr002"))
    .bind(base + Duration::milliseconds(10))
    .bind(&run_id)
    .bind(counter.next())
    .bind(&scope)
    .bind("shame")
    .bind(0.10_f64)
    .bind(0.25_f64)
    .bind(0.15_f64)
    .bind("outcome")
    .bind("contradicted")
    .bind(sqlx::types::Json(
        json!({"intervention_id": format!("{run_id}.iv002"),
                                   "classification": "contradicted",
                                   "confidence": "medium"}),
    ))
    .bind(sqlx::types::Json(json!([])))
    .bind(false)
    .execute(&pool)
    .await
    .unwrap();

    // Seed projection row at zero so the test scope exists.
    common::upsert_raw(&pool, &scope, "shame", 0.0, &json!({})).await;

    rebuild_projection(&pool, false).await.unwrap();

    let (value, evidence) = common::select_raw(&pool, &scope, "shame").await.unwrap();
    assert!(
        (value - 0.25).abs() < 1e-9,
        "rebuild should land on the latest new_value, got {value}"
    );
    assert_eq!(evidence["classification"], "contradicted");

    common::cleanup_run(&pool, &run_id).await;
    common::cleanup_scope(&pool, &scope).await;
}

#[tokio::test]
async fn rebuild_default_skips_disabled_events() {
    let _lock = REBUILD_LOCK.lock().await;
    let pool = common::shared_pool().await;
    let run_id = common::unique_run_id();
    let scope = common::unique_scope();

    let base = Utc::now();

    // Event 1: live, sets to 0.20.
    insert_raw_event(
        &pool,
        &run_id,
        1,
        base,
        &scope,
        "doubt",
        0.0,
        0.20,
        "outcome",
        "ignored",
        &format!("{run_id}.iv001"),
        "ignored",
        "medium",
        false,
    )
    .await;
    // Event 2: disabled, would set to 0.50 if replayed.
    insert_raw_event(
        &pool,
        &run_id,
        2,
        base + Duration::milliseconds(10),
        &scope,
        "doubt",
        0.20,
        0.50,
        "outcome",
        "harmful",
        &format!("{run_id}.iv002"),
        "harmful",
        "high",
        true,
    )
    .await;

    common::upsert_raw(&pool, &scope, "doubt", 0.0, &json!({})).await;

    // Default mode skips disabled.
    rebuild_projection(&pool, false).await.unwrap();
    let (value, _) = common::select_raw(&pool, &scope, "doubt").await.unwrap();
    assert!(
        (value - 0.20).abs() < 1e-9,
        "default rebuild must skip disabled events; got {value}"
    );

    // --include-disabled mode replays disabled too.
    rebuild_projection(&pool, true).await.unwrap();
    let (value, _) = common::select_raw(&pool, &scope, "doubt").await.unwrap();
    assert!(
        (value - 0.50).abs() < 1e-9,
        "include-disabled rebuild must apply disabled events; got {value}"
    );

    common::cleanup_run(&pool, &run_id).await;
    common::cleanup_scope(&pool, &scope).await;
}

#[tokio::test]
async fn rebuild_is_from_zero_when_event_log_is_empty_for_scope() {
    // Contract §10.5: rebuild MUST zero the projection before replay.
    // The smoke-purge scenario (handover.local.md 2026-05-29 daemon-pin #2):
    // operator deletes events from `satan_attribute_events`, then runs
    // rebuild — projection must collapse to zero for any scope whose
    // events are gone, NOT remain at the cached pre-purge value.
    //
    // §17.8 also pins that the zero-step resets `last_decay_at` to NULL —
    // the scheduler treats post-rebuild rows as "decay never ran" and
    // fires on the next hourly check. Asserted below by seeding a
    // non-NULL pre-state.
    let _lock = REBUILD_LOCK.lock().await;
    let pool = common::shared_pool().await;
    let scope = common::unique_scope();

    // Seed the projection at a non-zero value with NO matching events.
    // (Equivalent to the post-purge state: projection holds a stale
    // cached value, event log carries nothing for this scope.)
    common::upsert_raw(
        &pool,
        &scope,
        "shame",
        0.50,
        &json!({"intervention_id": "stale.iv001"}),
    )
    .await;
    // Pin a non-NULL `last_decay_at` so the post-rebuild NULL assertion
    // proves the zero-step actually cleared it (rather than the row
    // simply having been inserted with the column's NULL default).
    sqlx::query(
        "UPDATE satan_attributes SET last_decay_at = NOW()
         WHERE scope = $1 AND name = $2",
    )
    .bind(&scope)
    .bind("shame")
    .execute(&pool)
    .await
    .unwrap();

    let (value_before, _) = common::select_raw(&pool, &scope, "shame").await.unwrap();
    assert!(
        (value_before - 0.50).abs() < 1e-9,
        "pre-rebuild value should be the stale 0.50, got {value_before}"
    );

    rebuild_projection(&pool, false).await.unwrap();

    let (value_after, evidence_after) = common::select_raw(&pool, &scope, "shame").await.unwrap();
    assert!(
        value_after.abs() < 1e-9,
        "rebuild must zero projection rows whose events are absent; got {value_after}"
    );
    assert_eq!(
        evidence_after,
        json!({}),
        "rebuild must reset evidence_json on zero-step; got {evidence_after}"
    );

    let last_decay_after: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(
        "SELECT last_decay_at FROM satan_attributes WHERE scope = $1 AND name = $2",
    )
    .bind(&scope)
    .bind("shame")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        last_decay_after.is_none(),
        "rebuild must reset last_decay_at to NULL (§17.8); got {last_decay_after:?}"
    );

    common::cleanup_scope(&pool, &scope).await;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn insert_raw_event(
    pool: &sqlx::PgPool,
    run_id: &str,
    seq: i32,
    ts: chrono::DateTime<Utc>,
    scope: &str,
    name: &str,
    old: f64,
    new: f64,
    source: &str,
    reason: &str,
    iv_id: &str,
    classification: &str,
    confidence: &str,
    disabled: bool,
) {
    sqlx::query(
        "INSERT INTO satan_attribute_events
           (id, ts, run_id, seq, scope, name, old_value, new_value, delta,
            source, reason, evidence_json, caps_applied, disabled)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(format!("{run_id}.attr{seq:03}"))
    .bind(ts)
    .bind(run_id)
    .bind(seq)
    .bind(scope)
    .bind(name)
    .bind(old)
    .bind(new)
    .bind(new - old)
    .bind(source)
    .bind(reason)
    .bind(sqlx::types::Json(json!({
        "intervention_id": iv_id,
        "classification": classification,
        "confidence": confidence,
    })))
    .bind(sqlx::types::Json(json!([])))
    .bind(disabled)
    .execute(pool)
    .await
    .unwrap();
}
