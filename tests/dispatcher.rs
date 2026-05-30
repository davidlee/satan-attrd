//! Integration tests for the T-attr-1c dispatcher.
//!
//! Pure-function behaviour (delta table, confidence weighting, caps,
//! snapshot ordering, range_clamp, revision union) is covered in unit
//! tests under `src/dispatcher.rs`. This file exercises the DB-touching
//! pieces: gather_prior_actuals against the real event log + the revision
//! arithmetic that closes around it; full dispatch → insert_event round-
//! trip; the friction_cap forward-compat direct-store fixture.
//!
//! Requires Postgres reachable at `$DATABASE_URL`. Each test allocates a
//! unique run_id (and per-test intervention ids derived from it) so writes
//! never collide between parallel cases.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;

use chrono::{Duration, Utc};
use serde_json::json;

use satan_attrd::{
    ATTR_ORDER, AttributeName, Cap, Confidence, Counter, CueDimensions, EventInsert, OutcomeInput,
    OutcomeReason, RevisionInput, Scope, Snapshot, base_deltas, dispatch_outcome,
    dispatch_revision, gather_prior_actuals, insert_event, plan_for, weight_delta,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn input(run_id: &str, iv_id: &str, class: OutcomeReason, conf: Confidence) -> OutcomeInput {
    OutcomeInput {
        run_id: run_id.to_string(),
        ts: Utc::now(),
        intervention_id: iv_id.to_string(),
        classification: class,
        confidence: conf,
        cue: CueDimensions {
            intervention_kind: Some("notify".into()),
            related_motive_id: None,
            cue_handles: vec!["focus.sway:firefox".into()],
            related_trace_ids: vec![],
        },
        enabled: true,
        snapshot: Snapshot::default(),
        projection: HashMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Golden 15 — 5 classifications × 3 confidences. Assert weighted-delta
// arithmetic survives the dispatcher (per attribute, ignoring caps by
// using zero projection + zero snapshot so range_clamp + friction_cap
// stay silent).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn golden_15_outcome_deltas() {
    let pool = common::shared_pool().await;
    let run_id = common::unique_run_id();

    let classes = [
        OutcomeReason::Worked,
        OutcomeReason::Neutral,
        OutcomeReason::Ignored,
        OutcomeReason::Contradicted,
        OutcomeReason::Harmful,
    ];
    let confs = [Confidence::Low, Confidence::Medium, Confidence::High];

    let counter = Counter::new();
    let mut case_idx = 0;
    for class in classes {
        for conf in confs {
            case_idx += 1;
            let iv_id = format!("{run_id}.iv{case_idx:03}");
            let mut inp = input(&run_id, &iv_id, class, conf);
            // Seed every attribute at a mid-range value so positive deltas
            // never crash into range_clamp = 1.0 and negative never crash
            // into 0.0.
            for n in ATTR_ORDER {
                inp.projection.insert(n, 0.50);
            }
            let events = dispatch_outcome(&inp, &counter);

            // Affected count: non-zero entries in the §6 row for `class`.
            let row = base_deltas(class);
            let affected: Vec<_> = ATTR_ORDER
                .iter()
                .zip(row.iter())
                .filter(|(_, d)| **d != 0.0)
                .collect();
            assert_eq!(
                events.len(),
                affected.len(),
                "{class:?}/{conf:?}: event count mismatch"
            );

            // Per-attribute weighted delta (no caps triggered at mid-range).
            for (name, base) in affected {
                let expected_delta = weight_delta(*base, conf);
                let ev = events.iter().find(|e| e.name == *name).unwrap();
                assert!(
                    close(ev.old_value, 0.50),
                    "{class:?}/{conf:?} {name:?}: old"
                );
                assert!(
                    close(ev.new_value, 0.50 + expected_delta),
                    "{class:?}/{conf:?} {name:?}: new (expected {} got {})",
                    0.50 + expected_delta,
                    ev.new_value
                );
                assert!(
                    close(ev.delta(), expected_delta),
                    "{class:?}/{conf:?} {name:?}: delta (expected {} got {})",
                    expected_delta,
                    ev.delta()
                );
                assert!(
                    ev.caps_applied.is_empty(),
                    "{class:?}/{conf:?} {name:?}: caps fired unexpectedly: {:?}",
                    ev.caps_applied
                );

                // Round-trip the row through Postgres to make sure the
                // dispatcher's output is insertable end-to-end.
                insert_event(&pool, ev).await.unwrap();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pre-dispatch snapshot — caps in a single multi-attribute event use the
// frozen (doubt, shame), not the just-applied deltas from siblings.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn harmful_snapshot_freezes_friction_cap_inputs() {
    let pool = common::shared_pool().await;
    let run_id = common::unique_run_id();
    let iv_id = format!("{run_id}.iv001");

    // Pre-dispatch snapshot says doubt=shame=0 → friction bound = 1.0.
    // Within the same event, shame and doubt both rise by +0.30, but the
    // friction delta sees the FROZEN snapshot — not the just-raised
    // shame/doubt — so its cap input is bound=1.0 throughout. Harmful
    // friction is -0.30 anyway (cap doesn't fire on negative), so the
    // assertion is the simpler "no friction_cap appears".
    let mut inp = input(&run_id, &iv_id, OutcomeReason::Harmful, Confidence::Medium);
    inp.snapshot = Snapshot {
        doubt: 0.0,
        shame: 0.0,
    };
    inp.projection.insert(AttributeName::Friction, 0.60);
    inp.projection.insert(AttributeName::Shame, 0.10);
    inp.projection.insert(AttributeName::Doubt, 0.10);
    inp.projection.insert(AttributeName::Metamorphosis, 0.10);

    let counter = Counter::new();
    let events = dispatch_outcome(&inp, &counter);

    let friction = events
        .iter()
        .find(|e| e.name == AttributeName::Friction)
        .unwrap();
    assert!(friction.caps_applied.is_empty());
    assert!(close(friction.new_value, 0.30));

    // Insert all four for round-trip.
    for ev in &events {
        insert_event(&pool, ev).await.unwrap();
    }
}

// ---------------------------------------------------------------------------
// Range clamp — upper and lower, observed via integration round-trip.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn range_clamp_upper_via_integration() {
    let pool = common::shared_pool().await;
    let run_id = common::unique_run_id();
    let iv_id = format!("{run_id}.iv001");

    let mut inp = input(&run_id, &iv_id, OutcomeReason::Harmful, Confidence::High);
    // Shame at 0.95 + harmful/high theoretical = +0.30 (after upper clamp)
    // → new = 1.25 → range_clamp to 1.0.
    inp.projection.insert(AttributeName::Shame, 0.95);
    inp.projection.insert(AttributeName::Doubt, 0.20);
    inp.projection.insert(AttributeName::Friction, 0.30);
    inp.projection.insert(AttributeName::Metamorphosis, 0.10);

    let counter = Counter::new();
    let events = dispatch_outcome(&inp, &counter);
    let shame = events
        .iter()
        .find(|e| e.name == AttributeName::Shame)
        .unwrap();
    assert!(close(shame.new_value, 1.0));
    assert!(shame.caps_applied.contains(&Cap::RangeClamp));
    insert_event(&pool, shame).await.unwrap();
}

#[tokio::test]
async fn range_clamp_lower_via_integration() {
    let pool = common::shared_pool().await;
    let run_id = common::unique_run_id();
    let iv_id = format!("{run_id}.iv001");

    let mut inp = input(&run_id, &iv_id, OutcomeReason::Harmful, Confidence::High);
    // Friction at 0.05 + harmful/high delta = -0.30 (after lower clamp) →
    // new = -0.25 → range_clamp to 0.0.
    inp.projection.insert(AttributeName::Friction, 0.05);
    inp.projection.insert(AttributeName::Shame, 0.20);
    inp.projection.insert(AttributeName::Doubt, 0.20);
    inp.projection.insert(AttributeName::Metamorphosis, 0.10);

    let counter = Counter::new();
    let events = dispatch_outcome(&inp, &counter);
    let friction = events
        .iter()
        .find(|e| e.name == AttributeName::Friction)
        .unwrap();
    assert!(close(friction.new_value, 0.0));
    assert!(friction.caps_applied.contains(&Cap::RangeClamp));
    insert_event(&pool, friction).await.unwrap();
}

// ---------------------------------------------------------------------------
// Disable switch — event row written with disabled=true; the run loop will
// skip UPSERT. We assert that the disabled flag survives the DB round-trip
// and that the event row is queryable like any other.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disabled_emit_writes_event_row_with_disabled_true() {
    let pool = common::shared_pool().await;
    let run_id = common::unique_run_id();
    let iv_id = format!("{run_id}.iv001");

    let mut inp = input(
        &run_id,
        &iv_id,
        OutcomeReason::Contradicted,
        Confidence::Medium,
    );
    inp.enabled = false; // broker switch off
    inp.projection.insert(AttributeName::Shame, 0.30);
    inp.projection.insert(AttributeName::Doubt, 0.30);
    inp.projection.insert(AttributeName::Friction, 0.30);
    inp.projection.insert(AttributeName::Suspicion, 0.30);
    inp.projection.insert(AttributeName::Metamorphosis, 0.10);

    let counter = Counter::new();
    let events = dispatch_outcome(&inp, &counter);
    assert!(!events.is_empty());
    for ev in &events {
        assert!(ev.disabled, "every event must carry disabled=true");
        insert_event(&pool, ev).await.unwrap();
    }

    // gather_prior_actuals filters disabled — should return zero sums.
    let names: Vec<_> = events.iter().map(|e| e.name).collect();
    let priors = gather_prior_actuals(&pool, &iv_id, &names).await.unwrap();
    for (name, sum) in priors {
        assert!(
            close(sum, 0.0),
            "disabled events must not contribute to prior_actual sum ({name:?} = {sum})"
        );
    }
}

// ---------------------------------------------------------------------------
// Revision against actually-logged prior — the prior event was cap-clamped
// to +0.02 (not the theoretical +0.05). The revision must compute against
// +0.02, NOT +0.05.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn revision_uses_actually_logged_prior_delta() {
    let pool = common::shared_pool().await;
    let run_id = common::unique_run_id();
    let iv_id = format!("{run_id}.iv001");

    // 1. Seed a prior outcome event for `shame` with actual logged delta
    //    = +0.02 (simulating a cap-clamped +0.05 theoretical).
    let counter = Counter::new();
    let prior = EventInsert {
        run_id: run_id.clone(),
        seq: counter.next(),
        ts: Utc::now(),
        scope: Scope::Global,
        name: AttributeName::Shame,
        old_value: 0.98,
        new_value: 1.0,
        source: "outcome".into(),
        reason: "ignored".into(),
        evidence_json: json!({
            "intervention_id": iv_id,
            "classification": "ignored",
            "confidence": "medium",
        }),
        caps_applied: vec![Cap::RangeClamp],
        disabled: false,
    };
    insert_event(&pool, &prior).await.unwrap();

    // 2. Look up the actual prior delta via the helper.
    let priors = gather_prior_actuals(&pool, &iv_id, &[AttributeName::Shame])
        .await
        .unwrap();
    let actual = priors.get(&AttributeName::Shame).copied().unwrap();
    assert!(
        close(actual, 0.02),
        "expected actual prior +0.02, got {actual}"
    );

    // 3. Revise (ignored, medium) → (contradicted, medium). New theoretical
    //    for shame = +0.15. Revision delta = 0.15 - 0.02 = +0.13.
    let mut base = input(
        &run_id,
        &iv_id,
        OutcomeReason::Contradicted,
        Confidence::Medium,
    );
    // Seed a non-extreme projection so cap doesn't dominate the assertion.
    base.projection.insert(AttributeName::Shame, 0.50);
    base.projection.insert(AttributeName::Doubt, 0.30);
    base.projection.insert(AttributeName::Friction, 0.20);
    base.projection.insert(AttributeName::Suspicion, 0.20);
    base.projection.insert(AttributeName::Brooding, 0.10);
    let rev = RevisionInput {
        base,
        prior_classification: OutcomeReason::Ignored,
        prior_actuals: priors,
        revises: iv_id.clone(),
    };
    let revision_events = dispatch_revision(&rev, &counter);
    let shame = revision_events
        .iter()
        .find(|e| e.name == AttributeName::Shame)
        .unwrap();
    assert!(
        close(shame.delta(), 0.13),
        "revision delta should be theoretical(+0.15) - actual(+0.02) = +0.13, got {}",
        shame.delta()
    );
    // evidence carries prior_actual + revises.
    assert!(close(
        shame.evidence_json["prior_actual"].as_f64().unwrap(),
        0.02
    ));
    assert_eq!(shame.evidence_json["revises"], iv_id);
    // Theoretical-minus-theoretical would have been +0.10 — assert we did
    // NOT take that path.
    assert!(
        !close(shame.delta(), 0.10),
        "should NOT be theoretical-minus-theoretical +0.10"
    );

    insert_event(&pool, shame).await.unwrap();
}

// ---------------------------------------------------------------------------
// Revision chain — two prior revisions accumulate into the prior_actual
// sum used by the third revision.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn revision_chain_sums_prior_actuals() {
    let pool = common::shared_pool().await;
    let run_id = common::unique_run_id();
    let iv_id = format!("{run_id}.iv001");

    let counter = Counter::new();
    let base_ts = Utc::now();

    // Step 1: original (ignored, medium) → shame +0.05 (uncapped).
    let e1 = EventInsert {
        run_id: run_id.clone(),
        seq: counter.next(),
        ts: base_ts,
        scope: Scope::Global,
        name: AttributeName::Shame,
        old_value: 0.10,
        new_value: 0.15,
        source: "outcome".into(),
        reason: "ignored".into(),
        evidence_json: json!({
            "intervention_id": iv_id,
            "classification": "ignored",
            "confidence": "medium",
        }),
        caps_applied: vec![],
        disabled: false,
    };
    insert_event(&pool, &e1).await.unwrap();

    // Step 2: revision to (contradicted, medium) — revision_delta was
    // +0.10 (theoretical 0.15 - prior_actual 0.05).
    let e2 = EventInsert {
        run_id: run_id.clone(),
        seq: counter.next(),
        ts: base_ts + Duration::milliseconds(10),
        scope: Scope::Global,
        name: AttributeName::Shame,
        old_value: 0.15,
        new_value: 0.25,
        source: "outcome".into(),
        reason: "contradicted".into(),
        evidence_json: json!({
            "intervention_id": iv_id,
            "classification": "contradicted",
            "confidence": "medium",
            "revises": iv_id,
            "prior_actual": 0.05,
        }),
        caps_applied: vec![],
        disabled: false,
    };
    insert_event(&pool, &e2).await.unwrap();

    // Step 3: revise again to (harmful, medium). The actual delta history
    // for this iv_id sums to +0.05 + +0.10 = +0.15. Theoretical for
    // (harmful, medium) shame = +0.30. Revision delta should therefore be
    // +0.30 - +0.15 = +0.15.
    let priors = gather_prior_actuals(&pool, &iv_id, &[AttributeName::Shame])
        .await
        .unwrap();
    let chained = priors.get(&AttributeName::Shame).copied().unwrap();
    assert!(
        close(chained, 0.15),
        "chained prior_actual should sum to +0.15, got {chained}"
    );

    let mut base = input(&run_id, &iv_id, OutcomeReason::Harmful, Confidence::Medium);
    base.projection.insert(AttributeName::Shame, 0.25);
    base.projection.insert(AttributeName::Doubt, 0.10);
    base.projection.insert(AttributeName::Friction, 0.20);
    base.projection.insert(AttributeName::Metamorphosis, 0.10);
    let rev = RevisionInput {
        base,
        prior_classification: OutcomeReason::Contradicted,
        prior_actuals: priors,
        revises: iv_id.clone(),
    };
    let events = dispatch_revision(&rev, &counter);
    let shame = events
        .iter()
        .find(|e| e.name == AttributeName::Shame)
        .unwrap();
    assert!(
        close(shame.delta(), 0.15),
        "step-3 revision delta = harmful_theoretical(0.30) - chained_prior(0.15) = 0.15, got {}",
        shame.delta()
    );
}

// ---------------------------------------------------------------------------
// Friction cap forward-compat fixture — no v1 outcome can produce a
// positive friction delta, so the cap is exercised by a direct-store
// synthesised positive delta through plan_for.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn friction_cap_via_direct_store_helper() {
    let pool = common::shared_pool().await;
    let run_id = common::unique_run_id();

    // Synthesised positive friction delta — would never arise from §6
    // outcomes but exercises the cap path the schema reserves.
    let snap = Snapshot {
        doubt: 0.40,
        shame: 0.40,
    };
    let plan = plan_for(AttributeName::Friction, 0.25, 0.10, snap);
    assert!(plan.caps_applied.contains(&Cap::FrictionCap));
    assert!(close(plan.new_value, 0.20)); // bound = 1 - 0.4 - 0.4 = 0.20

    let counter = Counter::new();
    let ev = EventInsert {
        run_id: run_id.clone(),
        seq: counter.next(),
        ts: Utc::now(),
        scope: Scope::Global,
        name: AttributeName::Friction,
        old_value: plan.old_value,
        new_value: plan.new_value,
        source: "outcome".into(),
        // No real outcome reason produces positive friction; the test row
        // pretends to be a forward-compat fixture only. The validator
        // boundary lives in the broker — this row never reaches it.
        reason: "ignored".into(),
        evidence_json: json!({
            "intervention_id": format!("{run_id}.iv001"),
            "classification": "ignored",
            "confidence": "high",
        }),
        caps_applied: plan.caps_applied.clone(),
        disabled: false,
    };
    insert_event(&pool, &ev).await.unwrap();
}
