//! Storage API — `satan_attributes` UPSERT + `satan_attribute_events` INSERT
//! + lookup + per-run seq counter + projection rebuild.
//!
//! Trust boundary: the broker's audit validator (contract §5.1) is upstream;
//! callers here (dispatcher in T-attr-1c, rebuild driver below) are trusted
//! to produce coherent input. The store does the database work + the
//! mechanical structural checks (delta = new - old, range, name parses)
//! that protect the schema from caller bugs.

use std::sync::atomic::{AtomicI32, Ordering};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::types::Json;

use crate::error::{Error, Result};
use crate::types::{AttributeName, Cap, Scope};

// ---------------------------------------------------------------------------
// Per-run seq counter
// ---------------------------------------------------------------------------

/// Monotonic seq counter scoped to a single SATAN run.
///
/// `next()` returns 1 on the first call, then 2, 3, ... The daemon allocates
/// one Counter per `run_id` (in the LISTENer's per-run map). Resetting between
/// runs is the caller's responsibility — Counter does not see run_id.
#[derive(Debug, Default)]
pub struct Counter {
    inner: AtomicI32,
}

impl Counter {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: AtomicI32::new(0),
        }
    }

    /// Allocate the next seq value. Returns 1, 2, 3, ... Wrapping at
    /// `i32::MAX` panics (a single SATAN run emitting >2B events is a bug,
    /// not a recoverable state).
    pub fn next(&self) -> i32 {
        let next = self.inner.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        assert!(next > 0, "attribute-event seq overflow within a single run");
        next
    }

    /// Current allocated count without incrementing (test helper).
    pub fn peek(&self) -> i32 {
        self.inner.load(Ordering::SeqCst)
    }
}

#[must_use]
pub fn format_event_id(run_id: &str, seq: i32) -> String {
    format!("{run_id}.attr{seq:03}")
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct AttributeRow {
    pub scope: String,
    pub name: String,
    pub value: f64,
    pub updated_at: DateTime<Utc>,
    pub evidence_json: Value,
    /// Idle-decay scheduler guard (contract §17.8). `None` means "decay has
    /// never run for this row" — fresh post-migration insert or post-rebuild
    /// reset. `Some(ts)` is the wallclock of the last successful decay tick;
    /// the scheduler fires when `now - ts ≥ 24h`.
    pub last_decay_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    pub id: String,
    pub ts: DateTime<Utc>,
    pub run_id: String,
    pub seq: i32,
    pub scope: String,
    pub name: String,
    pub old_value: f64,
    pub new_value: f64,
    pub delta: f64,
    pub source: String,
    pub reason: String,
    pub evidence_json: Value,
    pub caps_applied: Value,
    pub disabled: bool,
}

// ---------------------------------------------------------------------------
// Event insertion
// ---------------------------------------------------------------------------

/// Owned struct describing one `attribute.delta_applied` event ready for
/// `satan_attribute_events` insertion.
#[derive(Debug, Clone)]
pub struct EventInsert {
    pub run_id: String,
    pub seq: i32,
    pub ts: DateTime<Utc>,
    pub scope: Scope,
    pub name: AttributeName,
    pub old_value: f64,
    pub new_value: f64,
    pub source: String,
    pub reason: String,
    pub evidence_json: Value,
    pub caps_applied: Vec<Cap>,
    pub disabled: bool,
}

impl EventInsert {
    #[must_use]
    pub fn delta(&self) -> f64 {
        self.new_value - self.old_value
    }

    /// Structural sanity check. Returns the first violation found, or `Ok(())`.
    fn validate(&self) -> Result<()> {
        if !(0.0..=1.0).contains(&self.old_value) {
            return Err(Error::ValueOutOfRange(self.old_value));
        }
        if !(0.0..=1.0).contains(&self.new_value) {
            return Err(Error::ValueOutOfRange(self.new_value));
        }
        Ok(())
    }

    #[must_use]
    pub fn event_id(&self) -> String {
        format_event_id(&self.run_id, self.seq)
    }

    fn caps_json(&self) -> Value {
        Value::Array(
            self.caps_applied
                .iter()
                .map(|c| Value::String(c.as_str().to_string()))
                .collect(),
        )
    }
}

/// Insert one row into `satan_attribute_events`. Returns the formatted id.
///
/// # Errors
///
/// Returns `Error::ValueOutOfRange` if `old`/`new` are outside `[0, 1]`, or a
/// Sqlx error on database failure (notably `UNIQUE (run_id, seq)` collisions
/// surface as `sqlx::Error::Database` with code `23505`).
pub async fn insert_event(pool: &PgPool, ev: &EventInsert) -> Result<String> {
    ev.validate()?;
    let id = ev.event_id();
    let delta = ev.delta();
    let caps = ev.caps_json();

    sqlx::query(
        "INSERT INTO satan_attribute_events
           (id, ts, run_id, seq, scope, name, old_value, new_value, delta,
            source, reason, evidence_json, caps_applied, disabled)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(&id)
    .bind(ev.ts)
    .bind(&ev.run_id)
    .bind(ev.seq)
    .bind(ev.scope.as_str())
    .bind(ev.name.as_str())
    .bind(ev.old_value)
    .bind(ev.new_value)
    .bind(delta)
    .bind(&ev.source)
    .bind(&ev.reason)
    .bind(Json(&ev.evidence_json))
    .bind(Json(&caps))
    .bind(ev.disabled)
    .execute(pool)
    .await?;

    Ok(id)
}

// ---------------------------------------------------------------------------
// Projection UPSERT
// ---------------------------------------------------------------------------

/// Upsert one attribute value. Caller passes the `evidence_json` from the
/// source event so the projection's last-update provenance is recorded
/// per contract §4.1.
///
/// # Errors
///
/// Returns `Error::ValueOutOfRange` if `value` is outside `[0, 1]`, or a
/// Sqlx error on database failure.
pub async fn upsert_attribute(
    pool: &PgPool,
    scope: Scope,
    name: AttributeName,
    value: f64,
    evidence_json: &Value,
    updated_at: DateTime<Utc>,
) -> Result<()> {
    if !(0.0..=1.0).contains(&value) {
        return Err(Error::ValueOutOfRange(value));
    }

    sqlx::query(
        "INSERT INTO satan_attributes (scope, name, value, updated_at, evidence_json)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (scope, name)
         DO UPDATE SET value = EXCLUDED.value,
                       updated_at = EXCLUDED.updated_at,
                       evidence_json = EXCLUDED.evidence_json",
    )
    .bind(scope.as_str())
    .bind(name.as_str())
    .bind(value)
    .bind(updated_at)
    .bind(Json(evidence_json))
    .execute(pool)
    .await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Lookups
// ---------------------------------------------------------------------------

/// Read one attribute's current value.
///
/// # Errors
///
/// Returns a Sqlx error on database failure. `Ok(None)` if the row is absent
/// (should not happen post-migration — the seed inserts all 8 — but a callsite
/// fetching a `(scope, name)` pair the seed did not cover gets `None`).
pub async fn lookup_attribute(
    pool: &PgPool,
    scope: Scope,
    name: AttributeName,
) -> Result<Option<AttributeRow>> {
    let row: Option<(
        String,
        String,
        f64,
        DateTime<Utc>,
        Json<Value>,
        Option<DateTime<Utc>>,
    )> = sqlx::query_as(
        "SELECT scope, name, value, updated_at, evidence_json, last_decay_at
         FROM satan_attributes
         WHERE scope = $1 AND name = $2",
    )
    .bind(scope.as_str())
    .bind(name.as_str())
    .fetch_optional(pool)
    .await?;

    Ok(row.map(
        |(scope, name, value, updated_at, ev, last_decay_at)| AttributeRow {
            scope,
            name,
            value,
            updated_at,
            evidence_json: ev.0,
            last_decay_at,
        },
    ))
}

/// Prior `attribute.delta_applied` events for the same intervention id (any
/// scope, one attribute name). Ordered by `(ts, run_id, seq)` ascending —
/// the natural replay order from contract §10.
///
/// Used by the T-attr-1c dispatcher's §6.2 revision algorithm. The expression
/// index from migration 0007 keeps this cheap.
///
/// # Errors
///
/// Returns a Sqlx error on database failure.
pub async fn lookup_prior_events_by_intervention(
    pool: &PgPool,
    intervention_id: &str,
    name: AttributeName,
) -> Result<Vec<EventRow>> {
    let rows: Vec<(
        String,
        DateTime<Utc>,
        String,
        i32,
        String,
        String,
        f64,
        f64,
        f64,
        String,
        String,
        Json<Value>,
        Json<Value>,
        bool,
    )> = sqlx::query_as(
        "SELECT id, ts, run_id, seq, scope, name, old_value, new_value, delta,
                source, reason, evidence_json, caps_applied, disabled
         FROM satan_attribute_events
         WHERE evidence_json->>'intervention_id' = $1
           AND name = $2
         ORDER BY ts, run_id, seq",
    )
    .bind(intervention_id)
    .bind(name.as_str())
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                ts,
                run_id,
                seq,
                scope,
                name,
                old_value,
                new_value,
                delta,
                source,
                reason,
                evidence_json,
                caps_applied,
                disabled,
            )| EventRow {
                id,
                ts,
                run_id,
                seq,
                scope,
                name,
                old_value,
                new_value,
                delta,
                source,
                reason,
                evidence_json: evidence_json.0,
                caps_applied: caps_applied.0,
                disabled,
            },
        )
        .collect())
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// Read one bool-valued row from `satan_attribute_settings`. Returns `default`
/// if the row is absent.
///
/// # Errors
///
/// Returns `Error::InvalidArgument` if the row exists but the JSONB `value` is
/// not a JSON boolean, or a Sqlx error on database failure.
pub async fn get_setting_bool(pool: &PgPool, name: &str, default: bool) -> Result<bool> {
    let row: Option<(Json<Value>,)> = sqlx::query_as(
        "SELECT value FROM satan_attribute_settings WHERE name = $1",
    )
    .bind(name)
    .fetch_optional(pool)
    .await?;

    match row {
        None => Ok(default),
        Some((Json(Value::Bool(b)),)) => Ok(b),
        Some((Json(other),)) => Err(Error::InvalidArgument(format!(
            "satan_attribute_settings[{name}].value is not a JSON bool: {other}"
        ))),
    }
}

/// Upsert one bool-valued row into `satan_attribute_settings`. `to_jsonb` casts
/// the bound `BOOL` to JSONB server-side so the stored value is `true`/`false`
/// rather than a stringified boolean.
///
/// # Errors
///
/// Returns a Sqlx error on database failure.
pub async fn set_setting_bool(pool: &PgPool, name: &str, value: bool) -> Result<()> {
    sqlx::query(
        "INSERT INTO satan_attribute_settings (name, value)
         VALUES ($1, to_jsonb($2::boolean))
         ON CONFLICT (name)
         DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()",
    )
    .bind(name)
    .bind(value)
    .execute(pool)
    .await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Rebuild
// ---------------------------------------------------------------------------

/// Replay the event log into `satan_attributes` — **from zero**.
///
/// Wraps the operation in a single transaction:
///   1. Zero every projection row (`value = 0.0`, `evidence_json = '{}'`).
///   2. Walk `satan_attribute_events ORDER BY ts, run_id, seq` and UPSERT
///      the final `new_value` per `(scope, name)`.
///
/// `include_disabled=false` (the default) skips rows with `disabled=true`
/// (contract §10.1); `true` replays every row (§10.2 — hypothetical
/// post-rollback state). Both modes zero first (contract §10.5).
///
/// Rationale (contract §10.5): the projection must be derivable from the
/// event log alone. Replay-on-top contaminates the result with whatever
/// projection state pre-existed — including pre-rebuild drift and values
/// left over from since-purged events. From-zero is the only way to make
/// `rebuild` idempotent and to make event-log-purge → rebuild yield zero.
///
/// Returns the number of events replayed (post-filter). Returns `0` if the
/// event log was purged — in that case rebuild leaves the projection at
/// `value = 0.0` for every row (which is the correct derivation from an
/// empty event log).
///
/// # Errors
///
/// Returns a Sqlx error on database failure. The transaction rolls back if
/// any step fails, leaving the prior projection state intact.
pub async fn rebuild_projection(pool: &PgPool, include_disabled: bool) -> Result<usize> {
    let mut tx = pool.begin().await?;

    // Step 1 (§10.5): zero every row before replay. `last_decay_at` resets
    // to NULL per §17.8 "Catch-up across migration / rebuild" — rebuild is
    // an operator-triggered reset where the projection must reflect the
    // event log alone, so the scheduler treats post-rebuild rows as
    // "decay never ran" and fires on the next hourly check.
    sqlx::query(
        "UPDATE satan_attributes
         SET value = 0.0,
             evidence_json = '{}'::jsonb,
             updated_at = NOW(),
             last_decay_at = NULL",
    )
    .execute(&mut *tx)
    .await?;

    // Step 2 (§10.1 / §10.2): walk events in deterministic order, UPSERT.
    let rows: Vec<(String, String, f64, Json<Value>, DateTime<Utc>)> = if include_disabled {
        sqlx::query_as(
            "SELECT scope, name, new_value, evidence_json, ts
             FROM satan_attribute_events
             ORDER BY ts, run_id, seq",
        )
        .fetch_all(&mut *tx)
        .await?
    } else {
        sqlx::query_as(
            "SELECT scope, name, new_value, evidence_json, ts
             FROM satan_attribute_events
             WHERE disabled = false
             ORDER BY ts, run_id, seq",
        )
        .fetch_all(&mut *tx)
        .await?
    };

    let count = rows.len();
    for (scope, name, new_value, evidence, ts) in rows {
        sqlx::query(
            "INSERT INTO satan_attributes (scope, name, value, updated_at, evidence_json)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (scope, name)
             DO UPDATE SET value = EXCLUDED.value,
                           updated_at = EXCLUDED.updated_at,
                           evidence_json = EXCLUDED.evidence_json",
        )
        .bind(&scope)
        .bind(&name)
        .bind(new_value)
        .bind(ts)
        .bind(&evidence.0)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(count)
}

// ---------------------------------------------------------------------------
// Convenience helpers
// ---------------------------------------------------------------------------

/// Build an `evidence_json` blob for `source = outcome` events that carries
/// the contract §3.1 / §6.2.1 cue-dimension fields. Helper for tests + the
/// T-attr-1c dispatcher.
#[must_use]
pub fn outcome_evidence_json(
    intervention_id: &str,
    classification: &str,
    confidence: &str,
    intervention_kind: Option<&str>,
    related_motive_id: Option<&str>,
    cue_handles: &[&str],
    related_trace_ids: &[&str],
) -> Value {
    json!({
        "intervention_id": intervention_id,
        "classification": classification,
        "confidence": confidence,
        "intervention_kind": intervention_kind,
        "related_motive_id": related_motive_id,
        "cue_handles": cue_handles,
        "related_trace_ids": related_trace_ids,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn counter_starts_at_one_and_monotonic() {
        let c = Counter::new();
        assert_eq!(c.next(), 1);
        assert_eq!(c.next(), 2);
        assert_eq!(c.next(), 3);
        assert_eq!(c.peek(), 3);
    }

    #[test]
    fn event_id_format() {
        assert_eq!(format_event_id("r1", 7), "r1.attr007");
        assert_eq!(format_event_id("r1", 999), "r1.attr999");
        assert_eq!(format_event_id("r1", 1000), "r1.attr1000");
    }

    #[test]
    fn event_insert_delta() {
        let ev = EventInsert {
            run_id: "r1".into(),
            seq: 1,
            ts: Utc::now(),
            scope: Scope::Global,
            name: AttributeName::Shame,
            old_value: 0.10,
            new_value: 0.25,
            source: "outcome".into(),
            reason: "contradicted".into(),
            evidence_json: json!({}),
            caps_applied: vec![],
            disabled: false,
        };
        assert!((ev.delta() - 0.15).abs() < 1e-9);
    }

    #[test]
    fn event_insert_rejects_out_of_range() {
        let mut ev = EventInsert {
            run_id: "r1".into(),
            seq: 1,
            ts: Utc::now(),
            scope: Scope::Global,
            name: AttributeName::Shame,
            old_value: 0.0,
            new_value: 1.2,
            source: "outcome".into(),
            reason: "harmful".into(),
            evidence_json: json!({}),
            caps_applied: vec![],
            disabled: false,
        };
        assert!(matches!(ev.validate(), Err(Error::ValueOutOfRange(_))));
        ev.new_value = 1.0;
        ev.old_value = -0.1;
        assert!(matches!(ev.validate(), Err(Error::ValueOutOfRange(_))));
    }

    #[test]
    fn outcome_evidence_json_shape() {
        let ev = outcome_evidence_json(
            "r1.iv001",
            "contradicted",
            "medium",
            Some("notify"),
            None,
            &["focus.sway:firefox"],
            &["t1", "t2"],
        );
        assert_eq!(ev["intervention_id"], "r1.iv001");
        assert_eq!(ev["classification"], "contradicted");
        assert_eq!(ev["confidence"], "medium");
        assert_eq!(ev["intervention_kind"], "notify");
        assert!(ev["related_motive_id"].is_null());
        assert_eq!(ev["cue_handles"][0], "focus.sway:firefox");
        assert_eq!(ev["related_trace_ids"][1], "t2");
    }
}
