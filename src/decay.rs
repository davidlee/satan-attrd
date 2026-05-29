//! Idle-decay scheduler (design-contract §8 + §17.8).
//!
//! T-attr-2c lands the **infrastructure**: the `Clock` integration, the
//! `tokio::time::interval` driver, and the read-side `check_due` that
//! identifies rows pending decay. **No firing yet** — each tick logs the
//! would-decay set and returns; T-attr-2d wires the synthesise + dispatch
//! + `last_decay_at` bump path on top of this skeleton.
//!
//! Per §17.8:
//!   * 4 negative-pole targets: shame, doubt, brooding, metamorphosis.
//!   * Hourly check, daily fire — `(now - last_decay_at) ≥ 24h` OR
//!     `last_decay_at IS NULL`.
//!   * Single-tick catch-up across daemon downtime; the `days_since_last`
//!     field preserves the gap for observability when T-attr-2d ships.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::clock::Clock;
use crate::error::Result;
use crate::types::AttributeName;

/// The 4 negative-pole attributes subject to idle decay. Positive-pole
/// attributes (curiosity, hunger, suspicion, friction) are explicitly
/// excluded — see theme doc T-attr-2-decay.md §"Considered and rejected".
pub const DECAY_TARGETS: [AttributeName; 4] = [
    AttributeName::Shame,
    AttributeName::Doubt,
    AttributeName::Brooding,
    AttributeName::Metamorphosis,
];

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

#[derive(Debug)]
pub struct DecayScheduler<C: Clock> {
    pool: PgPool,
    clock: Arc<C>,
    scope: String,
}

impl<C: Clock + 'static> DecayScheduler<C> {
    #[must_use]
    pub fn new(pool: PgPool, clock: Arc<C>, scope: String) -> Self {
        Self { pool, clock, scope }
    }

    /// Identify rows whose `last_decay_at` is NULL or older than 24h.
    /// Pure read — does not mutate state. T-attr-2d turns the result
    /// into dispatched decay events.
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

    /// One scheduler tick. Identifies due rows, logs the set, returns the
    /// count. T-attr-2c stops here; T-attr-2d will dispatch synthetic
    /// `(source=maintenance, reason=idle_decay)` events from inside this
    /// method.
    ///
    /// # Errors
    ///
    /// Returns a Sqlx error on database failure.
    pub async fn tick(&self) -> Result<usize> {
        let due = self.check_due().await?;
        if !due.is_empty() {
            let names: Vec<&str> = due.iter().map(|r| r.name.as_str()).collect();
            tracing::info!(
                count = due.len(),
                names = ?names,
                "decay tick: rows due (T-attr-2c skeleton — no firing yet)"
            );
        } else {
            tracing::debug!("decay tick: nothing due");
        }
        Ok(due.len())
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
    /// T-attr-2d can promote tick failures without changing the
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
