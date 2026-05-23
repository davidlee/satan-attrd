//! satan-attrd — SATAN attribute layer daemon.
//!
//! The daemon owns the attribute state projection + event log, consumes
//! intervention outcome events from the broker, applies the §6 outcome→delta
//! table per [`docs/satan/attributes/design-contract.md`] in `~/.emacs.d`, and
//! RPCs `attribute.delta_applied` audit events back to the broker for
//! transcript writing.
//!
//! Scaffold milestone (T-attr-1a-locus). No modules yet. T-attr-1b adds
//! `migrate`, `pool`, `store`, `error`.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
