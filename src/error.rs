//! Error model.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("migration failed: {0}")]
    Migration(String),

    #[error("attribute not found: scope={scope} name={name}")]
    AttributeNotFound { scope: String, name: String },

    #[error("invalid attribute name: {0}")]
    InvalidAttributeName(String),

    #[error("invalid source: {0}")]
    InvalidSource(String),

    #[error("invalid reason for source={source_name}: {reason}")]
    InvalidReason { source_name: String, reason: String },

    #[error("invalid scope: {0}")]
    InvalidScope(String),

    #[error("value out of range [0, 1]: {0}")]
    ValueOutOfRange(f64),

    #[error("delta mismatch: new - old != delta (old={old} new={new} delta={delta})")]
    DeltaMismatch { old: f64, new: f64, delta: f64 },

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error(
        "decay seq collision on run_id={run_id} seq={seq}: the per-UTC-day \
         counter reset and re-emitted a persisted seq (likely a daemon restart \
         mid-day while attribute updates were disabled, so last_decay_at was \
         never bumped). Deferred structural fix: T-attr-2f counter-resume-on-restart."
    )]
    DecaySeqCollision { run_id: String, seq: i32 },

    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
