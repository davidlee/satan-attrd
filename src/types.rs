//! Wire types — attribute names, scope, source, reason.
//!
//! The design contract pins these as closed enums:
//!
//! - 8 attributes (contract §2)
//! - scope vocabulary (contract §3 — v1 writes only `Global`)
//! - 6 reserved sources (contract §5)
//! - per-source reason enums (contract §6 lists the `outcome` reasons; others
//!   land with T-attr-1e PRs)

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Attribute name (contract §2)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributeName {
    Curiosity,
    Hunger,
    Suspicion,
    Doubt,
    Friction,
    Shame,
    Brooding,
    Metamorphosis,
}

impl AttributeName {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Curiosity => "curiosity",
            Self::Hunger => "hunger",
            Self::Suspicion => "suspicion",
            Self::Doubt => "doubt",
            Self::Friction => "friction",
            Self::Shame => "shame",
            Self::Brooding => "brooding",
            Self::Metamorphosis => "metamorphosis",
        }
    }

    /// All 8 in canonical order (matches the migration seed).
    pub const ALL: [Self; 8] = [
        Self::Curiosity,
        Self::Hunger,
        Self::Suspicion,
        Self::Doubt,
        Self::Friction,
        Self::Shame,
        Self::Brooding,
        Self::Metamorphosis,
    ];
}

impl fmt::Display for AttributeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AttributeName {
    type Err = crate::error::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "curiosity" => Ok(Self::Curiosity),
            "hunger" => Ok(Self::Hunger),
            "suspicion" => Ok(Self::Suspicion),
            "doubt" => Ok(Self::Doubt),
            "friction" => Ok(Self::Friction),
            "shame" => Ok(Self::Shame),
            "brooding" => Ok(Self::Brooding),
            "metamorphosis" => Ok(Self::Metamorphosis),
            other => Err(crate::error::Error::InvalidAttributeName(other.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Scope (contract §3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Global,
    // Episode, MotiveBias — reserved for T-attr-2 additive overlays.
    //   patterns_attributes.design_note.md "Optional later: motive-local bias"
}

impl Scope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Scope {
    type Err = crate::error::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "global" => Ok(Self::Global),
            other => Err(crate::error::Error::InvalidScope(other.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Source (contract §5 — closed enum, reservation-vs-implementation)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// IMPLEMENTED in T-attr-1c.
    Outcome,
    /// IMPLEMENTED in T-attr-1d-hc.
    Hippocampus,
    /// RESERVED; T-attr-1e.
    Percept,
    /// RESERVED; T-attr-1e.
    Resonance,
    /// IMPLEMENTED in T-attr-1e-sensor.
    Sensor,
    /// IMPLEMENTED in T-attr-2d.
    Maintenance,
    /// RESERVED; T-attr-1e.
    ToolError,
    /// RESERVED; out of T-attr-1.
    Manual,
}

impl Source {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Outcome => "outcome",
            Self::Hippocampus => "hippocampus",
            Self::Percept => "percept",
            Self::Resonance => "resonance",
            Self::Sensor => "sensor",
            Self::Maintenance => "maintenance",
            Self::ToolError => "tool_error",
            Self::Manual => "manual",
        }
    }

    /// True for sources whose `reason` enum is fixed in the contract today.
    #[must_use]
    pub const fn is_implemented(self) -> bool {
        matches!(
            self,
            Self::Outcome | Self::Hippocampus | Self::Sensor | Self::Maintenance
        )
    }

    /// True for sources that support outcome revision (§6.2).
    #[must_use]
    pub const fn supports_revision(self) -> bool {
        matches!(self, Self::Outcome)
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Source {
    type Err = crate::error::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "outcome" => Ok(Self::Outcome),
            "hippocampus" => Ok(Self::Hippocampus),
            "percept" => Ok(Self::Percept),
            "resonance" => Ok(Self::Resonance),
            "sensor" => Ok(Self::Sensor),
            "maintenance" => Ok(Self::Maintenance),
            "tool_error" => Ok(Self::ToolError),
            "manual" => Ok(Self::Manual),
            other => Err(crate::error::Error::InvalidSource(other.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Outcome reasons (contract §6)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeReason {
    Worked,
    Neutral,
    Ignored,
    Contradicted,
    Harmful,
}

impl OutcomeReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Worked => "worked",
            Self::Neutral => "neutral",
            Self::Ignored => "ignored",
            Self::Contradicted => "contradicted",
            Self::Harmful => "harmful",
        }
    }
}

impl fmt::Display for OutcomeReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for OutcomeReason {
    type Err = crate::error::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "worked" => Ok(Self::Worked),
            "neutral" => Ok(Self::Neutral),
            "ignored" => Ok(Self::Ignored),
            "contradicted" => Ok(Self::Contradicted),
            "harmful" => Ok(Self::Harmful),
            other => Err(crate::error::Error::InvalidReason {
                source_name: Source::Outcome.as_str().to_string(),
                reason: other.to_string(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Hippocampus reasons (contract §6H)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HippocampusReason {
    Written,
    Overwritten,
    Deleted,
    Renamed,
    Searched,
    TraceMarked,
}

impl HippocampusReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Written => "written",
            Self::Overwritten => "overwritten",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Searched => "searched",
            Self::TraceMarked => "trace_marked",
        }
    }
}

impl fmt::Display for HippocampusReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HippocampusReason {
    type Err = crate::error::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "written" => Ok(Self::Written),
            "overwritten" => Ok(Self::Overwritten),
            "deleted" => Ok(Self::Deleted),
            "renamed" => Ok(Self::Renamed),
            "searched" => Ok(Self::Searched),
            "trace_marked" => Ok(Self::TraceMarked),
            other => Err(crate::error::Error::InvalidReason {
                source_name: Source::Hippocampus.as_str().to_string(),
                reason: other.to_string(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Sensor reasons (contract §6S)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorReason {
    SegmentBacklog,
    TypingActive,
    TypingIdle,
}

impl SensorReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SegmentBacklog => "segment_backlog",
            Self::TypingActive => "typing_active",
            Self::TypingIdle => "typing_idle",
        }
    }
}

impl fmt::Display for SensorReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SensorReason {
    type Err = crate::error::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "segment_backlog" => Ok(Self::SegmentBacklog),
            "typing_active" => Ok(Self::TypingActive),
            "typing_idle" => Ok(Self::TypingIdle),
            other => Err(crate::error::Error::InvalidReason {
                source_name: Source::Sensor.as_str().to_string(),
                reason: other.to_string(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Maintenance reasons (contract §6M)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceReason {
    IdleDecay,
}

impl MaintenanceReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IdleDecay => "idle_decay",
        }
    }
}

impl fmt::Display for MaintenanceReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MaintenanceReason {
    type Err = crate::error::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "idle_decay" => Ok(Self::IdleDecay),
            other => Err(crate::error::Error::InvalidReason {
                source_name: Source::Maintenance.as_str().to_string(),
                reason: other.to_string(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Cap names (contract §7)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cap {
    /// `friction ≤ max(0, 1 - doubt - shame)` per brief §1.
    FrictionCap,
    /// `new` clamped to `[0, 1]`.
    RangeClamp,
}

impl Cap {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FrictionCap => "friction_cap",
            Self::RangeClamp => "range_clamp",
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::single_element_loop,
    reason = "test fixtures: unwrap on known-good roundtrip values; single-element loop is idiomatic test for exhaustiveness pattern"
)]
mod tests {
    use super::*;

    #[test]
    fn attribute_name_round_trip() {
        for name in AttributeName::ALL {
            let s = name.as_str();
            assert_eq!(AttributeName::from_str(s).unwrap(), name);
        }
    }

    #[test]
    fn attribute_name_rejects_unknown() {
        assert!(AttributeName::from_str("rage").is_err());
    }

    #[test]
    fn source_implementation_status() {
        assert!(Source::Outcome.is_implemented());
        assert!(Source::Hippocampus.is_implemented());
        assert!(Source::Sensor.is_implemented());
        assert!(Source::Maintenance.is_implemented());
        assert!(!Source::Percept.is_implemented());
        assert!(!Source::Resonance.is_implemented());
        assert!(!Source::ToolError.is_implemented());
        assert!(!Source::Manual.is_implemented());
    }

    #[test]
    fn source_round_trip() {
        for src in [
            Source::Outcome,
            Source::Percept,
            Source::Resonance,
            Source::Sensor,
            Source::Maintenance,
            Source::ToolError,
            Source::Manual,
        ] {
            assert_eq!(Source::from_str(src.as_str()).unwrap(), src);
        }
    }

    #[test]
    fn outcome_reason_round_trip() {
        for r in [
            OutcomeReason::Worked,
            OutcomeReason::Neutral,
            OutcomeReason::Ignored,
            OutcomeReason::Contradicted,
            OutcomeReason::Harmful,
        ] {
            assert_eq!(OutcomeReason::from_str(r.as_str()).unwrap(), r);
        }
    }

    #[test]
    fn hippocampus_reason_round_trip() {
        for r in [
            HippocampusReason::Written,
            HippocampusReason::Overwritten,
            HippocampusReason::Deleted,
            HippocampusReason::Renamed,
            HippocampusReason::Searched,
            HippocampusReason::TraceMarked,
        ] {
            assert_eq!(HippocampusReason::from_str(r.as_str()).unwrap(), r);
        }
    }

    #[test]
    fn sensor_reason_round_trip() {
        for r in [
            SensorReason::SegmentBacklog,
            SensorReason::TypingActive,
            SensorReason::TypingIdle,
        ] {
            assert_eq!(SensorReason::from_str(r.as_str()).unwrap(), r);
        }
    }

    #[test]
    fn maintenance_reason_round_trip() {
        for r in [MaintenanceReason::IdleDecay] {
            assert_eq!(MaintenanceReason::from_str(r.as_str()).unwrap(), r);
        }
    }

    #[test]
    fn scope_round_trip() {
        assert_eq!(Scope::from_str("global").unwrap(), Scope::Global);
        assert!(Scope::from_str("episode").is_err());
    }
}
