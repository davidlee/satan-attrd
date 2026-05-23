//! satan-attrd — SATAN attribute layer daemon.
//!
//! The daemon owns the attribute state projection + event log, consumes
//! intervention outcome events from the broker, applies the §6 outcome→delta
//! table per `docs/satan/attributes/design-contract.md` in `~/.emacs.d`, and
//! RPCs `attribute.delta_applied` audit events back to the broker for
//! transcript writing.

pub mod error;
pub mod migrate;
pub mod pool;
pub mod store;
pub mod types;

pub use error::{Error, Result};
pub use store::{
    AttributeRow, Counter, EventInsert, EventRow, format_event_id, insert_event,
    lookup_attribute, lookup_prior_events_by_intervention, outcome_evidence_json,
    rebuild_projection, upsert_attribute,
};
pub use types::{AttributeName, Cap, OutcomeReason, Scope, Source};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
