//! Idle-decay scheduler (design-contract §8 + §17.8).
//!
//! T-attr-2c lands the infrastructure: `Clock` integration, hourly
//! `tokio::time::interval` driver, and the read-side `check_due`.
//! T-attr-2d extends `tick` to apply decay — the same shape as the
//! source-event loop (run_loop.rs:586-601), driven from the scheduler
//! rather than a LISTEN payload.
//!
//! Per §17.8:
//!   * 4 negative-pole targets: shame, doubt, brooding, metamorphosis.
//!   * Hourly check, daily fire — `(now - last_decay_at) ≥ 24h` OR
//!     `last_decay_at IS NULL`.
//!   * Single-tick catch-up across daemon downtime; the
//!     `days_since_last` field preserves the gap for observability.
//!
//! Per §17.5 (resolved §15 Q7, option A):
//!   * `attribute_updates_enabled` is read from `satan_attribute_settings`
//!     at the start of each tick and threaded into `MaintenanceInput.enabled`
//!     → `EventInsert.disabled`.
//!   * Disabled rows: event written with `disabled=true`; UPSERT skipped;
//!     `last_decay_at` NOT bumped (so the next-enabled tick still fires).
//!
//! Per §4.2:
//!   * `run_id` is `maintenance:<YYYY-MM-DD>` (UTC).
//!   * `seq` is allocated from a per-UTC-day `Counter` rotated on UTC day
//!     roll, keeping seq monotonic within each run_id without cross-cutting
//!     the run-loop's LRU.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;

use crate::clock::Clock;
use crate::dispatcher::{MaintenanceInput, Snapshot, dispatch_maintenance};
use crate::error::{Error, Result};
use crate::rpc;
use crate::run_loop;
use crate::store::{self, Counter, bump_last_decay_at, get_setting_bool};
use crate::types::{AttributeName, MaintenanceReason};

/// The `(run_id, seq)` collision a mid-day scheduler restart provokes: the
/// per-UTC-day counter resets to zero and re-emits seqs already persisted under
/// the same `run_id`. Only reachable on the disabled path, where `last_decay_at`
/// is never bumped so cold targets stay due across the restart. Because the
/// event `id` is derived from `(run_id, seq)`, the primary-key constraint trips
/// first; the `(run_id, seq)` unique constraint is the same violation by another
/// name. Either is mapped to a loud `Error::DecaySeqCollision`; structural fix
/// in T-attr-2f.
fn is_run_seq_collision(err: &Error) -> bool {
    matches!(
        err,
        Error::Sqlx(sqlx::Error::Database(db))
            if matches!(
                db.constraint(),
                Some("satan_attribute_events_pkey")
                    | Some("satan_attribute_events_run_id_seq_key")
            )
    )
}

/// The 4 negative-pole attributes subject to idle decay. Positive-pole
/// attributes (curiosity, hunger, suspicion, friction) are explicitly
/// excluded — see theme doc T-attr-2-decay.md §"Considered and rejected".
pub const DECAY_TARGETS: [AttributeName; 4] = [
    AttributeName::Shame,
    AttributeName::Doubt,
    AttributeName::Brooding,
    AttributeName::Metamorphosis,
];

/// The synthetic per-UTC-day run_id for idle-decay events (§4.2). One per
/// UTC calendar day; `seq` is allocated from the day's `Counter`.
fn maintenance_run_id(day: NaiveDate) -> String {
    format!("maintenance:{}", day.format("%Y-%m-%d"))
}

/// Scheduler check cadence. Hourly bounds restart-jitter to <1h; the
/// per-row `last_decay_at` 24h guard prevents double-fires across hourly
/// checks.
pub const DECAY_TICK_INTERVAL: Duration = Duration::from_secs(3600);

/// Staleness threshold — a row fires when `(now - last_decay_at) ≥ 24h`.
#[must_use]
pub fn decay_threshold() -> chrono::Duration {
    chrono::Duration::hours(24)
}

#[derive(Debug, Clone, PartialEq)]
pub struct DueRow {
    pub name: AttributeName,
    pub value: f64,
    pub last_decay_at: Option<DateTime<Utc>>,
    /// Gap between `now` and `last_decay_at` in whole days. `None` when
    /// `last_decay_at` is NULL — i.e. "decay has never run for this row"
    /// (fresh post-migration insert, post-rebuild reset).
    pub days_since_last: Option<i64>,
}

/// Per-UTC-day Counter state. The (day, counter) pair rotates whenever
/// `tick` observes a date roll, so each `maintenance:<utc-day>` run_id
/// gets monotonic `seq` from zero. Held under a `std::sync::Mutex` —
/// the critical section is the rotation check + Arc clone, never held
/// across `.await`.
#[derive(Debug)]
struct DayCounterState {
    day: NaiveDate,
    counter: Arc<Counter>,
}

#[derive(Debug)]
pub struct DecayScheduler<C: Clock> {
    pool: PgPool,
    clock: Arc<C>,
    scope: String,
    day_counter_state: Mutex<DayCounterState>,
}

impl<C: Clock + 'static> DecayScheduler<C> {
    #[must_use]
    pub fn new(pool: PgPool, clock: Arc<C>, scope: String) -> Self {
        // Sentinel day (MIN) guarantees the first tick rotates the counter
        // for whatever today is — resuming it from the persisted MAX(seq)+1
        // (see acquire_day_counter, §17.8 / T-attr-2f). Avoids snapshotting
        // wall-clock at construction time, which would diverge from
        // FakeClock-based tests, and keeps the resume query off the
        // construction path (it runs lazily on the first due tick).
        let day_counter_state = Mutex::new(DayCounterState {
            day: NaiveDate::MIN,
            counter: Arc::new(Counter::new()),
        });
        Self {
            pool,
            clock,
            scope,
            day_counter_state,
        }
    }

    /// Identify rows whose `last_decay_at` is NULL or older than 24h.
    /// Pure read — does not mutate state.
    ///
    /// # Errors
    ///
    /// Returns a Sqlx error on database failure.
    pub async fn check_due(&self) -> Result<Vec<DueRow>> {
        let names: Vec<&'static str> = DECAY_TARGETS.iter().map(|n| n.as_str()).collect();
        let rows: Vec<(String, f64, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT name, value, last_decay_at
             FROM satan_attributes
             WHERE scope = $1 AND name = ANY($2)",
        )
        .bind(&self.scope)
        .bind(&names)
        .fetch_all(&self.pool)
        .await?;

        let now = self.clock.now();
        let threshold = decay_threshold();
        let mut due = Vec::new();
        for (name_str, value, last_decay_at) in rows {
            let Ok(name) = name_str.parse::<AttributeName>() else {
                // Defence-in-depth: the IN-filter is by `name.as_str()` so
                // only known targets reach this branch. A row whose name
                // does not parse is a schema-bug and we skip rather than
                // taint the due set.
                tracing::warn!(name = %name_str, "decay: unparseable attribute name");
                continue;
            };
            let due_now = match last_decay_at {
                None => true,
                Some(t) => (now - t) >= threshold,
            };
            if due_now {
                let days_since_last = last_decay_at.map(|t| (now - t).num_days());
                due.push(DueRow {
                    name,
                    value,
                    last_decay_at,
                    days_since_last,
                });
            }
        }
        Ok(due)
    }

    /// Read current values for all DECAY_TARGETS at `self.scope`. Used
    /// to seed `MaintenanceInput.projection` + `Snapshot` once per tick
    /// (snapshot is §6.3 pre-dispatch; consistency within the tick is the
    /// guarantee — concurrent UPSERTs between this read and the apply
    /// loop are accepted, matching the source-event loop's behaviour).
    async fn read_target_values(&self) -> Result<HashMap<AttributeName, f64>> {
        let names: Vec<&'static str> = DECAY_TARGETS.iter().map(|n| n.as_str()).collect();
        let rows: Vec<(String, f64)> = sqlx::query_as(
            "SELECT name, value FROM satan_attributes
             WHERE scope = $1 AND name = ANY($2)",
        )
        .bind(&self.scope)
        .bind(&names)
        .fetch_all(&self.pool)
        .await?;
        let mut out = HashMap::with_capacity(rows.len());
        for (name_str, value) in rows {
            if let Ok(name) = name_str.parse::<AttributeName>() {
                out.insert(name, value);
            }
        }
        Ok(out)
    }

    /// Acquire (or rotate) the per-UTC-day Counter. On the fast path — still
    /// the same UTC day — returns the cached counter under a brief lock. On a
    /// date roll (including the first tick after construction) the counter
    /// resumes from `MAX(seq)+1` for that day's run_id, so a mid-day daemon
    /// restart while disabled no longer re-emits persisted `(run_id, seq)`
    /// pairs (§17.8 / T-attr-2f). The resume query runs before the lock; the
    /// sync critical section (compare day, swap, clone) is never held across
    /// `.await`.
    ///
    /// Mutex poisoning is recovered, not propagated: the guarded state is a
    /// date plus an `Arc<Counter>` with no invariant a panicking holder could
    /// corrupt, so reading the inner value beats aborting a tick.
    async fn acquire_day_counter(&self, today: NaiveDate) -> Result<Arc<Counter>> {
        {
            let guard = self
                .day_counter_state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if guard.day == today {
                return Ok(Arc::clone(&guard.counter));
            }
        }
        // Date rolled: resume past any seq a prior process persisted for
        // today's run_id before re-taking the lock to swap the counter in.
        let prior_max = store::max_seq_for_run(&self.pool, &maintenance_run_id(today)).await?;
        let mut guard = self
            .day_counter_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if guard.day != today {
            guard.day = today;
            guard.counter = Arc::new(Counter::resuming_from(prior_max.unwrap_or(0)));
        }
        Ok(Arc::clone(&guard.counter))
    }

    /// One scheduler tick. Reads `attribute_updates_enabled`, identifies
    /// due rows, dispatches one synthetic `(maintenance, idle_decay)`
    /// event per row, applies the EventInsert pattern (insert_event →
    /// conditional UPSERT → audit RPC → conditional `last_decay_at` bump).
    ///
    /// Returns the number of due rows processed (0 when nothing was due;
    /// the count when firing happened — including disabled rows whose
    /// events were still written).
    ///
    /// # Errors
    ///
    /// Returns a Sqlx error on database failure.
    pub async fn tick(&self) -> Result<usize> {
        let due = self.check_due().await?;
        if due.is_empty() {
            tracing::debug!("decay tick: nothing due");
            return Ok(0);
        }

        let now = self.clock.now();
        let today = now.date_naive();
        let run_id = maintenance_run_id(today);

        let enabled = get_setting_bool(&self.pool, "attribute_updates_enabled", true).await?;
        let projection = self.read_target_values().await?;
        let snapshot = Snapshot {
            doubt: projection
                .get(&AttributeName::Doubt)
                .copied()
                .unwrap_or(0.0),
            shame: projection
                .get(&AttributeName::Shame)
                .copied()
                .unwrap_or(0.0),
        };
        let counter = self.acquire_day_counter(today).await?;

        let count = due.len();
        for row in due {
            let input = MaintenanceInput {
                run_id: run_id.clone(),
                ts: now,
                reason: MaintenanceReason::IdleDecay,
                target: row.name,
                days_since_last: row.days_since_last.unwrap_or(0).max(0),
                enabled,
                snapshot: snapshot.clone(),
                projection: projection.clone(),
            };
            let events = dispatch_maintenance(&input, &counter);
            for ev in &events {
                if let Err(e) = store::insert_event(&self.pool, ev).await {
                    if is_run_seq_collision(&e) {
                        tracing::error!(
                            run_id = %run_id,
                            seq = ev.seq,
                            name = ev.name.as_str(),
                            "decay tick aborted: per-UTC-day seq counter collided \
                             with a persisted event — likely a daemon restart \
                             mid-day while attribute updates were disabled \
                             (last_decay_at never bumped). Deferred structural \
                             fix: T-attr-2f counter-resume-on-restart."
                        );
                        return Err(Error::DecaySeqCollision {
                            run_id: run_id.clone(),
                            seq: ev.seq,
                        });
                    }
                    return Err(e);
                }
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
                let audit_payload = run_loop::build_audit_payload(ev);
                rpc::enqueue_audit_event(&self.pool, &audit_payload).await?;
                if !ev.disabled {
                    bump_last_decay_at(&self.pool, ev.scope, ev.name, ev.ts).await?;
                }
            }
        }

        tracing::info!(
            count,
            enabled,
            run_id = %run_id,
            "decay tick: applied"
        );
        Ok(count)
    }

    /// Driver loop. `tokio::time::interval` fires every hour; each fire
    /// calls `tick()`. Tick errors are logged and do not break the loop —
    /// dropping a single tick is preferable to terminating the daemon.
    /// Never returns under normal operation.
    ///
    /// # Errors
    ///
    /// Returns only on a fatal error the loop cannot recover from. The
    /// current implementation does not have one — kept as `Result` so
    /// future changes can promote tick failures without changing the
    /// signature.
    pub async fn run(self) -> Result<()> {
        let mut interval = tokio::time::interval(DECAY_TICK_INTERVAL);
        // Don't burst-catch-up after a runtime delay — §8 single-tick
        // rule says decay across a multi-hour gap collapses to one tick,
        // not N. `Delay` skips missed ticks and schedules from now.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tracing::info!(
            interval_secs = DECAY_TICK_INTERVAL.as_secs(),
            "decay scheduler started"
        );
        loop {
            interval.tick().await;
            if let Err(e) = self.tick().await {
                tracing::error!(?e, "decay tick failed (continuing)");
            }
        }
    }
}
