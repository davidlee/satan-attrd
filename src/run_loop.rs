//! Daemon run loop — LISTEN `satan_outcome_inbox`, dispatch per §6 + §7,
//! enqueue `attribute.delta_applied` audit events, and surface broker
//! rejects from `satan_audit_replies`.
//!
//! The loop is single-threaded by design (`tokio` `current_thread` runtime in
//! `main.rs`). One source event is fully processed end-to-end before the next
//! notify is consumed — the §6.3 pre-dispatch snapshot is naturally coherent
//! without `SELECT FOR UPDATE`. A future multi-worker daemon MUST re-add
//! row-level locks on `satan_attributes`.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use sqlx::types::Json;
use tokio::select;

use crate::dispatcher::{
    self, ATTR_ORDER, Confidence, CueDimensions, OutcomeInput, RevisionInput, Snapshot,
};
use crate::error::{Error, Result};
use crate::rpc;
use crate::store::{self, Counter, EventInsert};
use crate::types::{AttributeName, OutcomeReason};
#[cfg(test)]
use crate::types::Scope;

// ---------------------------------------------------------------------------
// Per-run Counter LRU (contract §17.7)
// ---------------------------------------------------------------------------

/// Max concurrent active runs the daemon keeps a `Counter` for. Beyond this
/// the least-recently-touched run-id is evicted with `tracing::info!`.
pub const COUNTER_LRU_CAP: usize = 64;

/// Bounded LRU keyed by `run_id`. Hand-rolled (no extra dep) — cap is tiny so
/// the linear `Vec::iter().position` scan is cheaper than a hashmap-based LRU.
#[derive(Debug)]
pub struct LruCounterMap {
    cap: usize,
    order: VecDeque<String>,
    map: HashMap<String, Counter>,
}

impl LruCounterMap {
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            order: VecDeque::with_capacity(cap),
            map: HashMap::with_capacity(cap),
        }
    }

    /// Return a reference to the run's `Counter`, allocating a new zero
    /// counter on first sight of `run_id`. Touches LRU order; evicts on cap.
    pub fn get_or_create(&mut self, run_id: &str) -> &Counter {
        if let Some(pos) = self.order.iter().position(|s| s == run_id) {
            let touched = self.order.remove(pos).unwrap_or_default();
            self.order.push_back(touched);
        } else {
            if self.map.len() >= self.cap
                && let Some(evicted) = self.order.pop_front()
            {
                self.map.remove(&evicted);
                tracing::info!(
                    run_id = %evicted,
                    cap = self.cap,
                    "LRU-evicted per-run Counter (§17.7)"
                );
            }
            self.map.insert(run_id.to_string(), Counter::new());
            self.order.push_back(run_id.to_string());
        }
        self.map.get(run_id).expect("just inserted/touched")
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Parsed broker → daemon outcome payload
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct OutcomePayload {
    run_id: String,
    ts: DateTime<Utc>,
    intervention_id: String,
    classification: OutcomeReason,
    confidence: Confidence,
    cue: CueDimensions,
    is_revision: bool,
    revises: Option<String>,
    enabled: bool,
}

fn parse_outcome_payload(v: &Value) -> std::result::Result<OutcomePayload, String> {
    let obj = v.as_object().ok_or_else(|| "payload must be object".to_string())?;
    let get_str = |key: &str| -> std::result::Result<String, String> {
        obj.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("missing or non-string field: {key}"))
    };
    let run_id = get_str("run_id")?;
    let ts_str = get_str("ts")?;
    let ts: DateTime<Utc> = ts_str
        .parse::<DateTime<Utc>>()
        .map_err(|e| format!("ts parse failed: {e}"))?;
    let intervention_id = get_str("intervention_id")?;
    let classification_str = get_str("classification")?;
    let classification = classification_str
        .parse::<OutcomeReason>()
        .map_err(|e| format!("classification: {e}"))?;
    let confidence_str = get_str("confidence")?;
    let confidence =
        Confidence::parse(&confidence_str).map_err(|e| format!("confidence: {e}"))?;

    let evidence = obj
        .get("evidence")
        .ok_or_else(|| "missing evidence".to_string())?;
    let cue = parse_cue(evidence)?;

    let is_revision = obj
        .get("is_revision")
        .and_then(Value::as_bool)
        .ok_or_else(|| "missing or non-bool is_revision".to_string())?;
    let revises = obj
        .get("revises")
        .and_then(|x| if x.is_null() { None } else { x.as_str().map(str::to_string) });
    let enabled = obj
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| "missing or non-bool enabled".to_string())?;

    if is_revision && revises.is_none() {
        return Err("is_revision=true but revises is null/missing".to_string());
    }

    Ok(OutcomePayload {
        run_id,
        ts,
        intervention_id,
        classification,
        confidence,
        cue,
        is_revision,
        revises,
        enabled,
    })
}

fn parse_cue(evidence: &Value) -> std::result::Result<CueDimensions, String> {
    let obj = evidence
        .as_object()
        .ok_or_else(|| "evidence must be object".to_string())?;
    let intervention_kind = obj
        .get("intervention_kind")
        .and_then(|v| if v.is_null() { None } else { v.as_str().map(str::to_string) });
    let related_motive_id = obj
        .get("related_motive_id")
        .and_then(|v| if v.is_null() { None } else { v.as_str().map(str::to_string) });
    let cue_handles = obj
        .get("cue_handles")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let related_trace_ids = obj
        .get("related_trace_ids")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(CueDimensions {
        intervention_kind,
        related_motive_id,
        cue_handles,
        related_trace_ids,
    })
}

// ---------------------------------------------------------------------------
// Snapshot + projection read
// ---------------------------------------------------------------------------

async fn read_projection(
    pool: &PgPool,
) -> Result<(Snapshot, HashMap<AttributeName, f64>)> {
    let rows: Vec<(String, f64)> = sqlx::query_as(
        "SELECT name, value FROM satan_attributes WHERE scope = 'global'",
    )
    .fetch_all(pool)
    .await?;

    let mut proj: HashMap<AttributeName, f64> = HashMap::with_capacity(8);
    let mut doubt = 0.0;
    let mut shame = 0.0;
    for (name, value) in rows {
        let parsed: AttributeName = name.parse()?;
        if parsed == AttributeName::Doubt {
            doubt = value;
        }
        if parsed == AttributeName::Shame {
            shame = value;
        }
        proj.insert(parsed, value);
    }
    Ok((Snapshot { doubt, shame }, proj))
}

// ---------------------------------------------------------------------------
// Audit payload construction (daemon → broker)
// ---------------------------------------------------------------------------

fn build_audit_payload(ev: &EventInsert) -> Value {
    let mut payload = json!({
        "id": ev.event_id(),
        "ts": ev.ts.to_rfc3339(),
        "scope": ev.scope.as_str(),
        "name": ev.name.as_str(),
        "old": ev.old_value,
        "new": ev.new_value,
        "delta": ev.delta(),
        "source": ev.source,
        "reason": ev.reason,
        "evidence": ev.evidence_json,
        "caps_applied": ev.caps_applied
            .iter()
            .map(|c| Value::String(c.as_str().to_string()))
            .collect::<Vec<_>>(),
        "disabled": ev.disabled,
    });
    payload = rpc::with_schema_version(payload);
    payload
}

// ---------------------------------------------------------------------------
// Run loop
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct RunLoop {
    pool: PgPool,
    counters: LruCounterMap,
}

impl RunLoop {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            counters: LruCounterMap::new(COUNTER_LRU_CAP),
        }
    }

    /// Drain any pending inbox + reply rows once (process the backlog that
    /// accumulated while the daemon was down), then loop on LISTEN. Returns
    /// only on a fatal error from the listeners.
    pub async fn run(mut self) -> Result<()> {
        let mut outcome_listener = PgListener::connect_with(&self.pool).await?;
        outcome_listener.listen(rpc::OUTCOME_INBOX_CHANNEL).await?;
        let mut reply_listener = PgListener::connect_with(&self.pool).await?;
        reply_listener.listen(rpc::AUDIT_REPLY_CHANNEL).await?;
        tracing::info!(
            outcome = rpc::OUTCOME_INBOX_CHANNEL,
            reply = rpc::AUDIT_REPLY_CHANNEL,
            "LISTEN started"
        );

        self.drain_outcome_inbox().await?;
        self.drain_audit_replies().await?;

        loop {
            select! {
                ev = outcome_listener.recv() => {
                    let notification = ev?;
                    self.handle_outcome_notify(notification.payload()).await;
                }
                ev = reply_listener.recv() => {
                    let notification = ev?;
                    self.handle_reply_notify(notification.payload()).await;
                }
            }
        }
    }

    /// Drain all currently-pending rows in `satan_outcome_inbox` once.
    /// Test + bootstrap entry point — production `run()` calls this once
    /// before falling into the `select!` LISTEN loop.
    pub async fn drain_outcome_inbox(&mut self) -> Result<()> {
        let ids: Vec<(i32,)> = sqlx::query_as(
            "SELECT id FROM satan_outcome_inbox
             WHERE claimed_at IS NULL
             ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?;
        for (id,) in ids {
            if let Err(e) = self.process_outcome_row(id).await {
                tracing::error!(?e, id, "outcome dispatch failed (drain)");
            }
        }
        Ok(())
    }

    /// Drain all pending `satan_audit_replies` rows once. Production
    /// `run()` calls this on startup; tests call it directly.
    pub async fn drain_audit_replies(&mut self) -> Result<()> {
        let ids: Vec<(i32,)> =
            sqlx::query_as("SELECT inbox_id FROM satan_audit_replies ORDER BY inbox_id")
                .fetch_all(&self.pool)
                .await?;
        for (id,) in ids {
            if let Err(e) = self.process_reply_row(id).await {
                tracing::error!(?e, id, "audit reply handler failed (drain)");
            }
        }
        Ok(())
    }

    async fn handle_outcome_notify(&mut self, payload: &str) {
        let id: i32 = match payload.parse() {
            Ok(i) => i,
            Err(e) => {
                tracing::error!(?e, payload, "outcome_inbox: malformed notify payload");
                return;
            }
        };
        if let Err(e) = self.process_outcome_row(id).await {
            tracing::error!(?e, id, "outcome dispatch failed");
        }
    }

    async fn handle_reply_notify(&mut self, payload: &str) {
        let id: i32 = match payload.parse() {
            Ok(i) => i,
            Err(e) => {
                tracing::error!(?e, payload, "audit_reply: malformed notify payload");
                return;
            }
        };
        if let Err(e) = self.process_reply_row(id).await {
            tracing::error!(?e, id, "audit reply handler failed");
        }
    }

    async fn process_outcome_row(&mut self, id: i32) -> Result<()> {
        let row: Option<(Json<Value>,)> = sqlx::query_as(
            "UPDATE satan_outcome_inbox
                SET claimed_at = NOW()
              WHERE id = $1 AND claimed_at IS NULL
              RETURNING payload_json",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((Json(payload),)) = row else {
            return Ok(());
        };

        if let Err(e) = rpc::check_schema_major(&payload) {
            tracing::error!(error = %e, id, "outcome inbox: schema_version rejected");
            self.delete_outcome_row(id).await?;
            return Ok(());
        }

        let outcome = match parse_outcome_payload(&payload) {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(error = %e, id, "outcome inbox: parse failed");
                self.delete_outcome_row(id).await?;
                return Ok(());
            }
        };

        let (snapshot, projection) = read_projection(&self.pool).await?;

        let events = if outcome.is_revision {
            self.build_revision_events(&outcome, snapshot, projection).await?
        } else {
            let input = OutcomeInput {
                run_id: outcome.run_id.clone(),
                ts: outcome.ts,
                intervention_id: outcome.intervention_id.clone(),
                classification: outcome.classification,
                confidence: outcome.confidence,
                cue: outcome.cue.clone(),
                enabled: outcome.enabled,
                snapshot,
                projection,
            };
            let counter = self.counters.get_or_create(&outcome.run_id);
            dispatcher::dispatch_outcome(&input, counter)
        };

        for ev in &events {
            store::insert_event(&self.pool, ev).await?;
            if !ev.disabled {
                store::upsert_attribute(
                    &self.pool,
                    ev.scope,
                    ev.name,
                    ev.new_value,
                    &ev.evidence_json,
                    ev.ts,
                )
                .await?;
            }
            let audit_payload = build_audit_payload(ev);
            rpc::enqueue_audit_event(&self.pool, &audit_payload).await?;
        }

        self.delete_outcome_row(id).await?;
        Ok(())
    }

    async fn build_revision_events(
        &mut self,
        outcome: &OutcomePayload,
        snapshot: Snapshot,
        projection: HashMap<AttributeName, f64>,
    ) -> Result<Vec<EventInsert>> {
        let revises = outcome
            .revises
            .clone()
            .ok_or_else(|| Error::InvalidArgument("is_revision but no revises".into()))?;
        let prior_classification = self.derive_prior_classification(&outcome.intervention_id).await?;
        let names = union_affected(prior_classification, outcome.classification);
        let prior_actuals =
            dispatcher::gather_prior_actuals(&self.pool, &outcome.intervention_id, &names).await?;

        let base = OutcomeInput {
            run_id: outcome.run_id.clone(),
            ts: outcome.ts,
            intervention_id: outcome.intervention_id.clone(),
            classification: outcome.classification,
            confidence: outcome.confidence,
            cue: outcome.cue.clone(),
            enabled: outcome.enabled,
            snapshot,
            projection,
        };
        let counter = self.counters.get_or_create(&outcome.run_id);
        Ok(dispatcher::dispatch_revision(
            &RevisionInput {
                base,
                prior_classification,
                prior_actuals,
                revises,
            },
            counter,
        ))
    }

    /// Latest prior event for this intervention carries the most recent
    /// classification in its `reason` column. The contract uses
    /// `prior_classification` to compute the union of affected attributes —
    /// for a chain of revisions the latest revision *is* the "current prior",
    /// matching contract §6.2 step 2's "old + new classifications" semantics.
    async fn derive_prior_classification(
        &self,
        intervention_id: &str,
    ) -> Result<OutcomeReason> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT reason
               FROM satan_attribute_events
              WHERE evidence_json->>'intervention_id' = $1
              ORDER BY ts DESC, run_id DESC, seq DESC
              LIMIT 1",
        )
        .bind(intervention_id)
        .fetch_optional(&self.pool)
        .await?;
        let (reason_str,) = row.ok_or_else(|| {
            Error::InvalidArgument(format!(
                "revision but no prior attribute events for intervention {intervention_id}"
            ))
        })?;
        reason_str.parse::<OutcomeReason>()
    }

    async fn delete_outcome_row(&self, id: i32) -> Result<()> {
        sqlx::query("DELETE FROM satan_outcome_inbox WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn process_reply_row(&mut self, inbox_id: i32) -> Result<()> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT error_msg FROM satan_audit_replies WHERE inbox_id = $1",
        )
        .bind(inbox_id)
        .fetch_optional(&self.pool)
        .await?;
        if let Some((msg,)) = row {
            tracing::error!(
                inbox_id,
                error = %msg,
                "broker rejected attribute.delta_applied (§17.4 log+drop)"
            );
            sqlx::query("DELETE FROM satan_audit_replies WHERE inbox_id = $1")
                .bind(inbox_id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }
}

fn union_affected(a: OutcomeReason, b: OutcomeReason) -> Vec<AttributeName> {
    let mut names = Vec::with_capacity(8);
    let row_a = dispatcher::base_deltas(a);
    let row_b = dispatcher::base_deltas(b);
    for (idx, name) in ATTR_ORDER.iter().enumerate() {
        if row_a[idx] != 0.0 || row_b[idx] != 0.0 {
            names.push(*name);
        }
    }
    names
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn lru_evicts_oldest_run_id() {
        let mut lru = LruCounterMap::new(3);
        lru.get_or_create("r1");
        lru.get_or_create("r2");
        lru.get_or_create("r3");
        assert_eq!(lru.len(), 3);
        // r4 evicts r1.
        lru.get_or_create("r4");
        assert!(lru.map.contains_key("r4"));
        assert!(!lru.map.contains_key("r1"));
        assert!(lru.map.contains_key("r2"));
    }

    #[test]
    fn lru_touch_moves_to_recent() {
        let mut lru = LruCounterMap::new(3);
        lru.get_or_create("r1");
        lru.get_or_create("r2");
        lru.get_or_create("r3");
        // Touch r1 — now r2 is oldest.
        lru.get_or_create("r1");
        lru.get_or_create("r4");
        assert!(lru.map.contains_key("r1"));
        assert!(!lru.map.contains_key("r2"));
    }

    #[test]
    fn lru_counters_are_per_run() {
        let mut lru = LruCounterMap::new(4);
        let c1 = lru.get_or_create("r1").next();
        let c2 = lru.get_or_create("r1").next();
        let other = lru.get_or_create("r2").next();
        assert_eq!(c1, 1);
        assert_eq!(c2, 2);
        assert_eq!(other, 1);
    }

    #[test]
    fn parse_outcome_payload_full_first_emit() {
        let payload = json!({
            "schema_version": "1.0",
            "run_id": "20260524T120000Z-test-deadbeef",
            "ts": "2026-05-24T12:00:00Z",
            "intervention_id": "20260524T120000Z-test-deadbeef.iv001",
            "classification": "contradicted",
            "confidence": "medium",
            "evidence": {
                "intervention_kind": "ask",
                "related_motive_id": null,
                "cue_handles": ["foo:bar"],
                "related_trace_ids": ["t1"]
            },
            "is_revision": false,
            "revises": null,
            "enabled": true,
        });
        let o = parse_outcome_payload(&payload).unwrap();
        assert_eq!(o.run_id, "20260524T120000Z-test-deadbeef");
        assert_eq!(o.classification, OutcomeReason::Contradicted);
        assert_eq!(o.confidence, Confidence::Medium);
        assert!(!o.is_revision);
        assert!(o.enabled);
        assert_eq!(o.cue.cue_handles, vec!["foo:bar".to_string()]);
    }

    #[test]
    fn parse_outcome_payload_revision_requires_revises() {
        let payload = json!({
            "schema_version": "1.0",
            "run_id": "r",
            "ts": "2026-05-24T12:00:00Z",
            "intervention_id": "r.iv001",
            "classification": "harmful",
            "confidence": "high",
            "evidence": {
                "intervention_kind": null,
                "related_motive_id": null,
                "cue_handles": [],
                "related_trace_ids": []
            },
            "is_revision": true,
            "revises": null,
            "enabled": true,
        });
        let err = parse_outcome_payload(&payload).unwrap_err();
        assert!(err.contains("is_revision=true"));
    }

    #[test]
    fn parse_outcome_payload_rejects_bad_ts() {
        let payload = json!({
            "schema_version": "1.0",
            "run_id": "r",
            "ts": "not a timestamp",
            "intervention_id": "r.iv001",
            "classification": "worked",
            "confidence": "low",
            "evidence": {
                "cue_handles": [],
                "related_trace_ids": []
            },
            "is_revision": false,
            "enabled": true,
        });
        let err = parse_outcome_payload(&payload).unwrap_err();
        assert!(err.contains("ts parse failed"));
    }

    #[test]
    fn union_affected_covers_both_classifications() {
        // worked affects shame, doubt, hunger, brooding.
        // contradicted affects friction, shame, doubt, suspicion, metamorphosis.
        // Union: friction, shame, doubt, hunger, suspicion, brooding, metamorphosis (7).
        let u = union_affected(OutcomeReason::Worked, OutcomeReason::Contradicted);
        assert_eq!(u.len(), 7);
        assert!(u.contains(&AttributeName::Friction));
        assert!(u.contains(&AttributeName::Hunger));
        assert!(u.contains(&AttributeName::Suspicion));
        assert!(!u.contains(&AttributeName::Curiosity));
    }

    #[test]
    fn build_audit_payload_carries_schema_version() {
        let ev = EventInsert {
            run_id: "r".to_string(),
            seq: 1,
            ts: "2026-05-24T12:00:00Z".parse().unwrap(),
            scope: Scope::Global,
            name: AttributeName::Shame,
            old_value: 0.10,
            new_value: 0.25,
            source: "outcome".to_string(),
            reason: "contradicted".to_string(),
            evidence_json: json!({"intervention_id": "r.iv001"}),
            caps_applied: vec![],
            disabled: false,
        };
        let p = build_audit_payload(&ev);
        assert_eq!(p["schema_version"], json!("1.0"));
        assert_eq!(p["id"], json!("r.attr001"));
        assert_eq!(p["scope"], json!("global"));
        assert_eq!(p["name"], json!("shame"));
        assert_eq!(p["old"], json!(0.10));
        assert_eq!(p["new"], json!(0.25));
        assert!((p["delta"].as_f64().unwrap() - 0.15).abs() < 1e-9);
        assert_eq!(p["disabled"], json!(false));
        assert_eq!(p["caps_applied"], json!([]));
    }
}
