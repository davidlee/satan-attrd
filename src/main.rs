//! satan-attrd — SATAN attribute layer daemon entry point.
//!
//! Scaffolding only. T-attr-1b lands the migration + store; T-attr-1c
//! lands the dispatcher. See `HANDOVER.md`.

use std::process::ExitCode;

use tracing_subscriber::{EnvFilter, fmt};

fn main() -> ExitCode {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "satan-attrd scaffold; no daemon work yet");
    ExitCode::SUCCESS
}
