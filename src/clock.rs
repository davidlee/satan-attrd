//! Time abstraction for test-injectable scheduler logic.
//!
//! Decay tick decisions (§17.8) read wallclock through this trait so test
//! seams can drive controlled gaps without `tokio::time::pause` — `pause`
//! affects `tokio::time::Instant` but not `chrono::Utc::now()`, which is
//! what the contract specifies (`last_decay_at TIMESTAMPTZ`). Production
//! uses `SystemClock`; tests use `FakeClock` with `set` / `advance`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default, Clone, Copy)]
#[expect(
    clippy::module_name_repetitions,
    reason = "idiomatic name; distinct from trait"
)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Test clock. Shareable via `Clone` (cheap `Arc` bump); mutations through
/// any handle are visible to every holder. Available unconditionally —
/// integration tests link against the library crate as a separate binary
/// and would not see `#[cfg(test)]`-gated items.
#[derive(Debug, Clone)]
#[expect(
    clippy::module_name_repetitions,
    reason = "idiomatic name; test-only clock"
)]
pub struct FakeClock {
    now: Arc<Mutex<DateTime<Utc>>>,
}

impl FakeClock {
    #[must_use]
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: Arc::new(Mutex::new(now)),
        }
    }

    /// Guard recovers from poisoning: the mutex protects a plain timestamp
    /// with no invariant a panicking holder could corrupt, so reading the
    /// inner value is sounder than aborting a test run.
    fn guard(&self) -> MutexGuard<'_, DateTime<Utc>> {
        self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set(&self, now: DateTime<Utc>) {
        *self.guard() = now;
    }

    pub fn advance(&self, by: chrono::Duration) {
        *self.guard() += by;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.guard()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_clock_set_and_advance() {
        let t0 = Utc::now();
        let c = FakeClock::new(t0);
        assert_eq!(c.now(), t0);
        c.advance(chrono::Duration::hours(2));
        assert_eq!(c.now(), t0 + chrono::Duration::hours(2));
        let t1 = t0 + chrono::Duration::days(1);
        c.set(t1);
        assert_eq!(c.now(), t1);
    }

    #[test]
    fn fake_clock_shared_via_clone() {
        let c1 = FakeClock::new(Utc::now());
        let c2 = c1.clone();
        c1.advance(chrono::Duration::hours(1));
        assert_eq!(c1.now(), c2.now(), "clones share the underlying time");
    }
}
