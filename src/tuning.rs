//! Centralised tuning constants for attribute delta tables.
//!
//! All magnitude values, confidence multipliers, and per-source delta tables
//! live here so the knobs are discoverable and adjustable in one place.
//! `dispatcher.rs` imports from this module.

use crate::types::{AttributeName, HippocampusReason, OutcomeReason, SensorReason};

// ---------------------------------------------------------------------------
// Magnitude constants (design-contract §6)
// ---------------------------------------------------------------------------

pub const TINY: f64 = 0.025;
pub const SMALL: f64 = 0.05;
pub const MEDIUM: f64 = 0.15;
pub const HIGH: f64 = 0.30;

// ---------------------------------------------------------------------------
// Confidence multipliers (design-contract §6.1)
// ---------------------------------------------------------------------------

pub const CONFIDENCE_LOW: f64 = 0.5;
pub const CONFIDENCE_MEDIUM: f64 = 1.0;
pub const CONFIDENCE_HIGH: f64 = 1.5;

/// Upper-bound magnitude clamp for confidence-weighted deltas.
pub const CONFIDENCE_MAGNITUDE_CAP: f64 = HIGH;

// ---------------------------------------------------------------------------
// Canonical attribute order for delta table arrays
// ---------------------------------------------------------------------------

/// 8-element order: curiosity, friction, shame, doubt, hunger, suspicion,
/// brooding, metamorphosis. Every `base_deltas` / `hippocampus_base_deltas` /
/// `sensor_base_deltas` function returns `[f64; 8]` indexed by this order.
pub const ATTR_ORDER: [AttributeName; 8] = [
    AttributeName::Curiosity,
    AttributeName::Friction,
    AttributeName::Shame,
    AttributeName::Doubt,
    AttributeName::Hunger,
    AttributeName::Suspicion,
    AttributeName::Brooding,
    AttributeName::Metamorphosis,
];

// ---------------------------------------------------------------------------
// Outcome → attribute delta table (design-contract §6)
// ---------------------------------------------------------------------------

/// §6 base delta row for `reason`. Order matches `ATTR_ORDER` (8 elements).
///
/// Column order: curiosity, friction, shame, doubt, hunger, suspicion,
/// brooding, metamorphosis.
///
/// Curiosity is 0 for all outcome reasons — outcomes are about intervention
/// results, not evidence-gathering pressure.
#[must_use]
pub const fn outcome_base_deltas(reason: OutcomeReason) -> [f64; 8] {
    //                       curio  frict  shame   doubt  hunger  susp   brood  meta
    match reason {
        OutcomeReason::Worked => [0.0, 0.0, -TINY, -SMALL, -SMALL, 0.0, SMALL, 0.0],
        OutcomeReason::Neutral => [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        OutcomeReason::Ignored => [0.0, -SMALL, SMALL, SMALL, 0.0, 0.0, SMALL, 0.0],
        OutcomeReason::Contradicted => [0.0, -MEDIUM, MEDIUM, MEDIUM, 0.0, -SMALL, 0.0, SMALL],
        OutcomeReason::Harmful => [0.0, -HIGH, HIGH, HIGH, 0.0, 0.0, 0.0, MEDIUM],
    }
}

// ---------------------------------------------------------------------------
// Hippocampus → attribute delta table (design-contract §6H)
// ---------------------------------------------------------------------------

/// §6H.2 base delta row for hippocampus `reason`. Order matches `ATTR_ORDER`.
/// No confidence weighting — base deltas are final (§6H.3).
///
/// `TraceMarked` is a memory-substrate trace persistence event: curiosity
/// falls (the organism recorded what it found) and brooding falls (acted on
/// rumination pressure). Same substrate-agnostic "internal memory operation"
/// category as hippocampus file writes.
#[must_use]
pub const fn hippocampus_base_deltas(reason: HippocampusReason) -> [f64; 8] {
    //                            curio  frict  shame  doubt  hunger  susp   brood  meta
    match reason {
        HippocampusReason::Written | HippocampusReason::Renamed => {
            [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -TINY, 0.0]
        }
        HippocampusReason::Overwritten | HippocampusReason::Deleted => {
            [0.0, 0.0, -TINY, 0.0, 0.0, 0.0, -TINY, 0.0]
        }
        HippocampusReason::Searched => [0.0, 0.0, 0.0, 0.0, 0.0, TINY, 0.0, 0.0],
        HippocampusReason::TraceMarked => [-TINY, 0.0, 0.0, 0.0, 0.0, 0.0, -TINY, 0.0],
    }
}

// ---------------------------------------------------------------------------
// Sensor → attribute delta table (design-contract §6S)
// ---------------------------------------------------------------------------

/// §6S base delta row for sensor `reason`. Order matches `ATTR_ORDER`.
/// No confidence weighting — sensor signals are binary (§6S.3).
#[must_use]
pub const fn sensor_base_deltas(reason: SensorReason) -> [f64; 8] {
    //                          curio  frict  shame  doubt  hunger  susp  brood  meta
    match reason {
        SensorReason::SegmentBacklog => [SMALL, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        SensorReason::TypingActive => [0.0, 0.0, 0.0, 0.0, SMALL, 0.0, 0.0, 0.0],
        SensorReason::TypingIdle => [0.0, 0.0, 0.0, 0.0, TINY, 0.0, 0.0, 0.0],
    }
}
