//! Outcome → attribute delta dispatcher (T-attr-1c).
//!
//! Pure-function core:
//!
//! - `base_deltas` returns the contract §6 row for an `OutcomeReason`.
//! - `weight_delta` applies §6.1 confidence weighting with upper-bound
//!   magnitude clamp (no lower-bound clamp — `:low` is allowed to produce
//!   sub-`small` deltas).
//! - `plan_for` applies §7 caps (`friction_cap` + `range_clamp`) using a
//!   pre-dispatch `(doubt, shame)` snapshot per §6.3.
//! - `dispatch_outcome` (first emit) and `dispatch_revision` (compute
//!   against actually-logged prior deltas per §6.2) return ready-to-insert
//!   `EventInsert` rows.
//!
//! The dispatcher does NOT touch Postgres. The caller (run loop in main.rs)
//! reads the projection snapshot, walks the prior-event log to build the
//! `prior_actuals` map for revisions, allocates `seq` via `Counter`, calls
//! `store::insert_event` + (when not disabled) `store::upsert_attribute`,
//! and RPCs each event back to the broker for transcript writing
//! (contract §17.3 + §17.4).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::error::{Error, Result};
use crate::store::{Counter, EventInsert, lookup_prior_events_by_intervention};
use crate::types::{AttributeName, Cap, OutcomeReason, Scope, Source};

// ---------------------------------------------------------------------------
// Confidence (§6.1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    #[must_use]
    pub const fn weight(self) -> f64 {
        match self {
            Self::Low => 0.5,
            Self::Medium => 1.0,
            Self::High => 1.5,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    /// # Errors
    ///
    /// Returns `Error::InvalidArgument` if `s` is not one of `low|medium|high`.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            other => Err(Error::InvalidArgument(format!(
                "confidence must be low|medium|high, got {other}"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Pre-dispatch snapshot (§6.3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct Snapshot {
    pub doubt: f64,
    pub shame: f64,
}

// ---------------------------------------------------------------------------
// §6 base delta table
// ---------------------------------------------------------------------------

/// Canonical attribute order for `base_deltas`: friction, shame, doubt,
/// hunger, suspicion, brooding, metamorphosis. Column 1 (friction) is NOT
/// optional — readers must not skip it (contract §6 column-order note).
pub const ATTR_ORDER: [AttributeName; 7] = [
    AttributeName::Friction,
    AttributeName::Shame,
    AttributeName::Doubt,
    AttributeName::Hunger,
    AttributeName::Suspicion,
    AttributeName::Brooding,
    AttributeName::Metamorphosis,
];

/// §6 base delta row for `reason`. Order matches `ATTR_ORDER`.
///
/// The `worked shame = -0.025` entry is a deliberate sub-`small` exception
/// (contract §6 footnote 1). The `contradicted suspicion = -0.05` and
/// `harmful suspicion = 0` entries reflect global-only scope (§6 footnotes
/// 2 + 3) — per-pattern consequences live in pattern records, not here.
#[must_use]
pub const fn base_deltas(reason: OutcomeReason) -> [f64; 7] {
    match reason {
        OutcomeReason::Worked => [0.0, -0.025, -0.05, -0.05, 0.0, 0.05, 0.0],
        OutcomeReason::Neutral => [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        OutcomeReason::Ignored => [-0.05, 0.05, 0.05, 0.0, 0.0, 0.05, 0.0],
        OutcomeReason::Contradicted => [-0.15, 0.15, 0.15, 0.0, -0.05, 0.0, 0.05],
        OutcomeReason::Harmful => [-0.30, 0.30, 0.30, 0.0, 0.0, 0.0, 0.15],
    }
}

/// Returns the affected (non-zero base) attributes for one reason. Order is
/// `ATTR_ORDER` so the result is deterministic.
#[must_use]
pub fn affected(reason: OutcomeReason) -> Vec<AttributeName> {
    let row = base_deltas(reason);
    ATTR_ORDER
        .iter()
        .zip(row.iter())
        .filter_map(|(name, d)| if *d == 0.0 { None } else { Some(*name) })
        .collect()
}

/// §6.1 confidence weight + upper-bound magnitude clamp (`±0.30`). No
/// lower-bound clamp — `low` is allowed to produce sub-`small` deltas.
#[must_use]
pub fn weight_delta(base: f64, conf: Confidence) -> f64 {
    let w = base * conf.weight();
    w.clamp(-CONFIDENCE_MAGNITUDE_CAP, CONFIDENCE_MAGNITUDE_CAP)
}

const CONFIDENCE_MAGNITUDE_CAP: f64 = 0.30;

// ---------------------------------------------------------------------------
// Per-attribute plan
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct AttributePlan {
    pub name: AttributeName,
    pub old_value: f64,
    pub new_value: f64,
    pub delta: f64,
    pub caps_applied: Vec<Cap>,
}

/// Apply §7 caps and clamp. `delta_in` is the post-confidence-weighting
/// delta (or the post-revision delta for revisions). `old_value` is the
/// current projection value. `snap` is the pre-dispatch `(doubt, shame)`
/// snapshot used by `friction_cap`.
#[must_use]
pub fn plan_for(name: AttributeName, delta_in: f64, old_value: f64, snap: Snapshot) -> AttributePlan {
    let mut new_value = old_value + delta_in;
    let mut caps = Vec::new();

    // §7.1 friction_cap: only restrains positive friction deltas.
    if name == AttributeName::Friction && delta_in > 0.0 {
        let bound = (1.0 - snap.doubt - snap.shame).max(0.0);
        if new_value > bound {
            new_value = bound;
            caps.push(Cap::FrictionCap);
        }
    }

    // §7.2 range_clamp.
    let clamped = new_value.clamp(0.0, 1.0);
    if (clamped - new_value).abs() > f64::EPSILON {
        caps.push(Cap::RangeClamp);
        new_value = clamped;
    }

    let delta = new_value - old_value;

    AttributePlan {
        name,
        old_value,
        new_value,
        delta,
        caps_applied: caps,
    }
}

// ---------------------------------------------------------------------------
// Source-event inputs
// ---------------------------------------------------------------------------

/// All cue-dimension fields the daemon receives in the broker's outcome
/// source-event payload (contract §3.1). Carried through into the
/// `evidence_json` of each emitted `attribute.delta_applied` event.
#[derive(Debug, Clone, Default)]
pub struct CueDimensions {
    pub intervention_kind: Option<String>,
    pub related_motive_id: Option<String>,
    pub cue_handles: Vec<String>,
    pub related_trace_ids: Vec<String>,
}

/// Inputs to a first-emit outcome dispatch.
#[derive(Debug, Clone)]
pub struct OutcomeInput {
    pub run_id: String,
    pub ts: DateTime<Utc>,
    pub intervention_id: String,
    pub classification: OutcomeReason,
    pub confidence: Confidence,
    pub cue: CueDimensions,
    /// Broker's `attribute-updates-enabled` switch state. `false` means the
    /// daemon writes the event with `disabled=true` and skips the UPSERT
    /// (contract §9 + §17.5).
    pub enabled: bool,
    pub snapshot: Snapshot,
    /// Current projection value for every attribute the dispatcher will
    /// touch. Caller (run loop) reads these from `satan_attributes`.
    pub projection: HashMap<AttributeName, f64>,
}

/// Inputs to a revision dispatch. Carries the prior classification (so the
/// union of affected attrs covers both old + new) and the actually-logged
/// prior delta sums per affected attribute (contract §6.2).
#[derive(Debug, Clone)]
pub struct RevisionInput {
    pub base: OutcomeInput,
    pub prior_classification: OutcomeReason,
    pub prior_actuals: HashMap<AttributeName, f64>,
    /// The broker's `:revises` pointer (per outcome-semantics §9) — opaque
    /// to the daemon; passed through into evidence_json.
    pub revises: String,
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// First-emit dispatch (`intervention.outcome_classified`). Returns one
/// `EventInsert` per affected attribute (non-zero base in §6).
///
/// The `disabled` flag on each event reflects `input.enabled`; the caller
/// is responsible for skipping the UPSERT when `disabled=true`.
pub fn dispatch_outcome(input: &OutcomeInput, counter: &Counter) -> Vec<EventInsert> {
    let row = base_deltas(input.classification);
    let mut out = Vec::new();
    for (idx, name) in ATTR_ORDER.iter().enumerate() {
        let base = row[idx];
        if base == 0.0 {
            continue;
        }
        let weighted = weight_delta(base, input.confidence);
        let old = input.projection.get(name).copied().unwrap_or(0.0);
        let plan = plan_for(*name, weighted, old, input.snapshot);
        out.push(event_insert_from_plan(
            input,
            *name,
            &plan,
            input.classification,
            outcome_evidence(input, None),
            counter,
        ));
    }
    out
}

/// Revision dispatch (`intervention.outcome_revised`). For each attribute
/// in the union of affected attrs across `prior_classification` and
/// `base.classification`, compute the revision delta against the
/// actually-logged prior delta sum (contract §6.2 step 2c), apply caps +
/// clamp via the snapshot, and emit one event — even if the resulting
/// delta is 0 (audit-trail completeness per §6.2 closing paragraph).
pub fn dispatch_revision(input: &RevisionInput, counter: &Counter) -> Vec<EventInsert> {
    let base = &input.base;
    let new_row = base_deltas(base.classification);
    let prior_row = base_deltas(input.prior_classification);
    let mut out = Vec::new();
    for (idx, name) in ATTR_ORDER.iter().enumerate() {
        let new_base = new_row[idx];
        let prior_base = prior_row[idx];
        // Union of affected attrs: any attribute whose base is non-zero in
        // either the prior or the new classification.
        if new_base == 0.0 && prior_base == 0.0 {
            continue;
        }
        let new_theoretical = weight_delta(new_base, base.confidence);
        let prior_actual = input.prior_actuals.get(name).copied().unwrap_or(0.0);
        let revision_delta = new_theoretical - prior_actual;
        let old = base.projection.get(name).copied().unwrap_or(0.0);
        let plan = plan_for(*name, revision_delta, old, base.snapshot);
        out.push(event_insert_from_plan(
            base,
            *name,
            &plan,
            base.classification,
            outcome_evidence(
                base,
                Some(RevisionEvidence {
                    revises: input.revises.clone(),
                    prior_actual,
                }),
            ),
            counter,
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Evidence + EventInsert construction
// ---------------------------------------------------------------------------

struct RevisionEvidence {
    revises: String,
    prior_actual: f64,
}

fn outcome_evidence(input: &OutcomeInput, revision: Option<RevisionEvidence>) -> Value {
    let mut ev = json!({
        "intervention_id": input.intervention_id,
        "classification": input.classification.as_str(),
        "confidence": input.confidence.as_str(),
        "intervention_kind": input.cue.intervention_kind,
        "related_motive_id": input.cue.related_motive_id,
        "cue_handles": input.cue.cue_handles,
        "related_trace_ids": input.cue.related_trace_ids,
    });
    if let Some(rev) = revision {
        ev["revises"] = Value::String(rev.revises);
        ev["prior_actual"] = json!(rev.prior_actual);
    }
    ev
}

fn event_insert_from_plan(
    input: &OutcomeInput,
    name: AttributeName,
    plan: &AttributePlan,
    reason: OutcomeReason,
    evidence_json: Value,
    counter: &Counter,
) -> EventInsert {
    EventInsert {
        run_id: input.run_id.clone(),
        seq: counter.next(),
        ts: input.ts,
        scope: Scope::Global,
        name,
        old_value: plan.old_value,
        new_value: plan.new_value,
        source: Source::Outcome.as_str().to_string(),
        reason: reason.as_str().to_string(),
        evidence_json,
        caps_applied: plan.caps_applied.clone(),
        disabled: !input.enabled,
    }
}

// ---------------------------------------------------------------------------
// Prior-actuals lookup (helper for the run loop)
// ---------------------------------------------------------------------------

/// Sum the actually-logged prior deltas for `intervention_id` across each
/// attribute in `names`. Walks the event log via the §6.2.1 expression index
/// and sums per-attribute (so a chain of prior revisions collapses into one
/// number per attribute, per contract §6.2 step 2a).
///
/// Disabled rows are NOT summed — they did not affect the projection and
/// must not affect the revision arithmetic. (Hypothetical replay with
/// `--include-disabled` is a separate operator action; the live revision
/// path operates on the actual trajectory.)
///
/// # Errors
///
/// Returns a Sqlx error on database failure.
pub async fn gather_prior_actuals(
    pool: &PgPool,
    intervention_id: &str,
    names: &[AttributeName],
) -> Result<HashMap<AttributeName, f64>> {
    let mut out = HashMap::new();
    for name in names {
        let rows = lookup_prior_events_by_intervention(pool, intervention_id, *name).await?;
        let sum: f64 = rows.iter().filter(|r| !r.disabled).map(|r| r.delta).sum();
        out.insert(*name, sum);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Unit tests (pure functions — no DB)
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn confidence_weights_match_contract() {
        assert!(close(Confidence::Low.weight(), 0.5));
        assert!(close(Confidence::Medium.weight(), 1.0));
        assert!(close(Confidence::High.weight(), 1.5));
    }

    #[test]
    fn base_deltas_match_contract_table() {
        // Spot-check every row at a few positions.
        // friction, shame, doubt, hunger, suspicion, brooding, metamorphosis
        assert_eq!(
            base_deltas(OutcomeReason::Worked),
            [0.0, -0.025, -0.05, -0.05, 0.0, 0.05, 0.0]
        );
        assert_eq!(
            base_deltas(OutcomeReason::Neutral),
            [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
        );
        assert_eq!(
            base_deltas(OutcomeReason::Ignored),
            [-0.05, 0.05, 0.05, 0.0, 0.0, 0.05, 0.0]
        );
        assert_eq!(
            base_deltas(OutcomeReason::Contradicted),
            [-0.15, 0.15, 0.15, 0.0, -0.05, 0.0, 0.05]
        );
        assert_eq!(
            base_deltas(OutcomeReason::Harmful),
            [-0.30, 0.30, 0.30, 0.0, 0.0, 0.0, 0.15]
        );
    }

    #[test]
    fn affected_skips_zero_entries() {
        // worked → shame, doubt, hunger, brooding (4 non-zero).
        let aff = affected(OutcomeReason::Worked);
        assert_eq!(aff.len(), 4);
        assert!(aff.contains(&AttributeName::Shame));
        assert!(aff.contains(&AttributeName::Doubt));
        assert!(aff.contains(&AttributeName::Hunger));
        assert!(aff.contains(&AttributeName::Brooding));
        // neutral → nothing.
        assert_eq!(affected(OutcomeReason::Neutral).len(), 0);
        // harmful → friction, shame, doubt, metamorphosis (4 non-zero).
        let aff = affected(OutcomeReason::Harmful);
        assert_eq!(aff.len(), 4);
        assert!(aff.contains(&AttributeName::Metamorphosis));
    }

    #[test]
    fn weight_delta_upper_clamp_only() {
        // harmful shame at high: 0.30 * 1.5 = 0.45 → clamped to +0.30.
        assert!(close(weight_delta(0.30, Confidence::High), 0.30));
        // harmful friction at high: -0.30 * 1.5 = -0.45 → clamped to -0.30.
        assert!(close(weight_delta(-0.30, Confidence::High), -0.30));
        // contradicted shame at high: 0.15 * 1.5 = 0.225 → unchanged.
        assert!(close(weight_delta(0.15, Confidence::High), 0.225));
        // ignored shame at low: 0.05 * 0.5 = 0.025 (no lower-bound clamp).
        assert!(close(weight_delta(0.05, Confidence::Low), 0.025));
        // worked shame exception at low: -0.025 * 0.5 = -0.0125 (no floor).
        assert!(close(weight_delta(-0.025, Confidence::Low), -0.0125));
        // neutral stays zero.
        assert!(close(weight_delta(0.0, Confidence::High), 0.0));
    }

    #[test]
    fn plan_for_range_clamp_upper() {
        let plan = plan_for(
            AttributeName::Shame,
            0.30,
            0.85,
            Snapshot::default(),
        );
        assert!(close(plan.new_value, 1.0));
        assert!(plan.caps_applied.contains(&Cap::RangeClamp));
        // delta recomputed against the clamp.
        assert!(close(plan.delta, 0.15));
    }

    #[test]
    fn plan_for_range_clamp_lower() {
        let plan = plan_for(
            AttributeName::Friction,
            -0.30,
            0.10,
            Snapshot::default(),
        );
        assert!(close(plan.new_value, 0.0));
        assert!(plan.caps_applied.contains(&Cap::RangeClamp));
        assert!(close(plan.delta, -0.10));
    }

    #[test]
    fn plan_for_friction_cap_only_restrains_positive() {
        // Snapshot says doubt=0.4, shame=0.4 → friction bound = 0.2.
        let snap = Snapshot { doubt: 0.4, shame: 0.4 };
        // Positive friction delta exceeding bound → cap.
        let plan = plan_for(AttributeName::Friction, 0.25, 0.10, snap);
        assert!(close(plan.new_value, 0.2));
        assert!(plan.caps_applied.contains(&Cap::FrictionCap));
        // Negative friction delta always passes.
        let plan = plan_for(AttributeName::Friction, -0.10, 0.50, snap);
        assert!(close(plan.new_value, 0.40));
        assert!(!plan.caps_applied.contains(&Cap::FrictionCap));
    }

    #[test]
    fn plan_for_friction_cap_zero_when_inhibitors_exceed_one() {
        // doubt + shame > 1 → bound = max(0, negative) = 0.
        let snap = Snapshot { doubt: 0.7, shame: 0.5 };
        let plan = plan_for(AttributeName::Friction, 0.10, 0.05, snap);
        assert!(close(plan.new_value, 0.0));
        assert!(plan.caps_applied.contains(&Cap::FrictionCap));
    }

    fn input_for(class: OutcomeReason, conf: Confidence) -> OutcomeInput {
        OutcomeInput {
            run_id: "r1".into(),
            ts: Utc::now(),
            intervention_id: "r1.iv001".into(),
            classification: class,
            confidence: conf,
            cue: CueDimensions::default(),
            enabled: true,
            snapshot: Snapshot::default(),
            projection: HashMap::new(),
        }
    }

    #[test]
    fn dispatch_outcome_skips_zero_attrs() {
        let input = input_for(OutcomeReason::Worked, Confidence::Medium);
        let counter = Counter::new();
        let events = dispatch_outcome(&input, &counter);
        // worked has 4 non-zero entries.
        assert_eq!(events.len(), 4);
        for ev in &events {
            assert!(ev.name != AttributeName::Friction);
            assert!(ev.name != AttributeName::Suspicion);
            assert!(ev.name != AttributeName::Metamorphosis);
        }
    }

    #[test]
    fn dispatch_outcome_neutral_emits_nothing() {
        let input = input_for(OutcomeReason::Neutral, Confidence::Medium);
        let counter = Counter::new();
        assert!(dispatch_outcome(&input, &counter).is_empty());
    }

    #[test]
    fn dispatch_outcome_disabled_flag_propagates() {
        let mut input = input_for(OutcomeReason::Ignored, Confidence::Medium);
        input.enabled = false;
        let counter = Counter::new();
        let events = dispatch_outcome(&input, &counter);
        assert!(!events.is_empty());
        for ev in &events {
            assert!(ev.disabled, "disabled flag should propagate to every event");
        }
    }

    #[test]
    fn dispatch_outcome_seqs_monotonic() {
        let input = input_for(OutcomeReason::Harmful, Confidence::Medium);
        let counter = Counter::new();
        let events = dispatch_outcome(&input, &counter);
        let seqs: Vec<i32> = events.iter().map(|e| e.seq).collect();
        for w in seqs.windows(2) {
            assert!(w[1] > w[0], "seqs must be monotonic, got {seqs:?}");
        }
    }

    #[test]
    fn dispatch_outcome_snapshot_does_not_reflect_concurrent_deltas() {
        // Multi-attribute event ordering: friction cap reads snapshot
        // (doubt, shame), NOT the just-raised values from the same event.
        // For harmful at medium conf with old friction = 0.5 and snapshot
        // doubt = shame = 0, the bound = 1.0 — friction delta is negative
        // anyway so no cap fires; the assertion is that the snapshot is
        // used (we control doubt/shame via snapshot, not via concurrent
        // shame/doubt deltas in the same event).
        let mut input = input_for(OutcomeReason::Harmful, Confidence::Medium);
        input.snapshot = Snapshot { doubt: 0.0, shame: 0.0 };
        input.projection.insert(AttributeName::Friction, 0.50);
        input.projection.insert(AttributeName::Shame, 0.10);
        input.projection.insert(AttributeName::Doubt, 0.10);
        input.projection.insert(AttributeName::Metamorphosis, 0.10);
        let counter = Counter::new();
        let events = dispatch_outcome(&input, &counter);
        // Friction event: old=0.50, delta=-0.30, new=0.20, no cap.
        let friction = events
            .iter()
            .find(|e| e.name == AttributeName::Friction)
            .unwrap();
        assert!(close(friction.old_value, 0.50));
        assert!(close(friction.new_value, 0.20));
        assert!(friction.caps_applied.is_empty());
    }

    #[test]
    fn revision_uses_actual_prior_not_theoretical() {
        // Prior outcome (ignored, medium) wrote shame delta theoretically
        // +0.05. Suppose the cap clipped it to +0.02 (e.g. shame was 0.98
        // pre-write — clamp to 1.0). Revision to (contradicted, medium)
        // theoretical = +0.15.
        //
        //   theoretical-minus-theoretical: +0.15 - +0.05 = +0.10
        //   actual-vs-theoretical (correct): +0.15 - +0.02 = +0.13
        let mut base = input_for(OutcomeReason::Contradicted, Confidence::Medium);
        base.projection.insert(AttributeName::Shame, 1.0);
        base.projection.insert(AttributeName::Doubt, 0.5);
        base.projection.insert(AttributeName::Friction, 0.20);
        base.projection.insert(AttributeName::Brooding, 0.1);
        base.projection.insert(AttributeName::Suspicion, 0.1);
        let mut prior_actuals = HashMap::new();
        prior_actuals.insert(AttributeName::Shame, 0.02);
        prior_actuals.insert(AttributeName::Doubt, 0.05);
        prior_actuals.insert(AttributeName::Friction, -0.05);
        prior_actuals.insert(AttributeName::Brooding, 0.05);
        let input = RevisionInput {
            base,
            prior_classification: OutcomeReason::Ignored,
            prior_actuals,
            revises: "r1.iv001".into(),
        };
        let counter = Counter::new();
        let events = dispatch_revision(&input, &counter);

        // shame should appear once with delta_in = +0.15 - +0.02 = +0.13,
        // then range_clamp from old=1.0 → caps_applied contains RangeClamp.
        let shame = events
            .iter()
            .find(|e| e.name == AttributeName::Shame)
            .unwrap();
        // old=1.0, delta_in=+0.13 → new=1.13 → clamped to 1.0 → delta=0.
        assert!(close(shame.old_value, 1.0));
        assert!(close(shame.new_value, 1.0));
        assert!(shame.caps_applied.contains(&Cap::RangeClamp));
        // evidence carries prior_actual.
        assert!(close(
            shame.evidence_json["prior_actual"].as_f64().unwrap(),
            0.02
        ));
        assert_eq!(shame.evidence_json["revises"], "r1.iv001");
    }

    #[test]
    fn revision_union_covers_prior_only_attrs() {
        // Old: harmful (has metamorphosis = +0.15). New: worked (no
        // metamorphosis). Metamorphosis must appear in the revision output
        // even though it is zero in the new row.
        let mut base = input_for(OutcomeReason::Worked, Confidence::Medium);
        base.projection.insert(AttributeName::Metamorphosis, 0.20);
        let mut prior_actuals = HashMap::new();
        prior_actuals.insert(AttributeName::Metamorphosis, 0.15);
        let input = RevisionInput {
            base,
            prior_classification: OutcomeReason::Harmful,
            prior_actuals,
            revises: "r1.iv001".into(),
        };
        let counter = Counter::new();
        let events = dispatch_revision(&input, &counter);
        let meta = events
            .iter()
            .find(|e| e.name == AttributeName::Metamorphosis)
            .unwrap();
        // new_theoretical=0, prior_actual=0.15 → revision_delta = -0.15.
        // old=0.20 → new=0.05.
        assert!(close(meta.new_value, 0.05));
    }

    #[test]
    fn revision_no_change_still_emits_event() {
        // Same classification + same confidence + prior_actual matches
        // theoretical exactly → revision_delta = 0. Per contract §6.2
        // closing paragraph, the events still emit.
        let mut base = input_for(OutcomeReason::Ignored, Confidence::Medium);
        // Seed projection so 'old' lookups don't default to zero.
        for n in [
            AttributeName::Friction,
            AttributeName::Shame,
            AttributeName::Doubt,
            AttributeName::Brooding,
        ] {
            base.projection.insert(n, 0.10);
        }
        let mut prior_actuals = HashMap::new();
        prior_actuals.insert(AttributeName::Friction, -0.05);
        prior_actuals.insert(AttributeName::Shame, 0.05);
        prior_actuals.insert(AttributeName::Doubt, 0.05);
        prior_actuals.insert(AttributeName::Brooding, 0.05);
        let input = RevisionInput {
            base,
            prior_classification: OutcomeReason::Ignored,
            prior_actuals,
            revises: "r1.iv001".into(),
        };
        let counter = Counter::new();
        let events = dispatch_revision(&input, &counter);
        assert_eq!(events.len(), 4);
        for ev in &events {
            assert!(close(ev.delta(), 0.0), "expected zero delta, got {ev:?}");
        }
    }
}
