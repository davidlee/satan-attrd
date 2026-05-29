//! satan-attrd — SATAN attribute layer daemon.
//!
//! The daemon owns the attribute state projection + event log, consumes
//! intervention outcome events from the broker, applies the §6 outcome→delta
//! table per `docs/satan/attributes/design-contract.md` in `~/.emacs.d`, and
//! RPCs `attribute.delta_applied` audit events back to the broker for
//! transcript writing.

pub mod clock;
pub mod decay;
pub mod dispatcher;
pub mod error;
pub mod migrate;
pub mod notify_stream;
pub mod pool;
pub mod rpc;
pub mod run_loop;
pub mod store;
pub mod tuning;
pub mod types;

pub use clock::{Clock, FakeClock, SystemClock};
pub use decay::{DECAY_TARGETS, DECAY_TICK_INTERVAL, DecayScheduler, DueRow, decay_threshold};
pub use dispatcher::{
    ATTR_ORDER, AttributePlan, Confidence, CueDimensions, HippocampusInput, OutcomeInput,
    RevisionInput, SensorInput, Snapshot, affected, base_deltas, dispatch_hippocampus,
    dispatch_outcome, dispatch_revision, dispatch_sensor, gather_prior_actuals,
    hippocampus_base_deltas, plan_for, sensor_base_deltas, weight_delta,
};
pub use error::{Error, Result};
pub use store::{
    AttributeRow, Counter, EventInsert, EventRow, bump_last_decay_at, format_event_id,
    get_setting_bool, insert_event, lookup_attribute, lookup_prior_events_by_intervention,
    outcome_evidence_json, rebuild_projection, set_setting_bool, upsert_attribute,
};
pub use types::{
    AttributeName, Cap, HippocampusReason, MaintenanceReason, OutcomeReason, Scope, SensorReason,
    Source,
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
