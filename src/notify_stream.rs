//! `satan-attrd notify-stream <channel> [<channel>...]` — emit one JSON
//! line per LISTEN notification to stdout.
//!
//! Why this exists: psql in non-tty pipe mode buffers async notifications
//! until next stdin input, so an elisp consumer that only LISTENs (no
//! periodic queries) never sees notifies in real time. This subcommand
//! holds a real libpq connection (via sqlx/tokio-postgres), receives
//! notifications asynchronously, and writes them line-delimited so any
//! `make-process` consumer (broker dl-satan-*-listener.el) reads them
//! from a filter.
//!
//! Line shape:
//!
//! ```text
//! {"channel":"satan_audit_inbox","payload":"42"}
//! ```
//!
//! Lifecycle: runs until SIGTERM / SIGINT. Exits 0 on signal, non-zero
//! on listener error (caller restarts).
use std::io::Write;

use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::select;
use tokio::signal::unix::{SignalKind, signal};

use crate::{Error, Result};

/// Line-delimited JSON to stdout; runs until SIGTERM/SIGINT.
///
/// # Errors
///
/// Returns an error if no channels provided, LISTEN setup fails, or
/// stdout write/flush fails. Exits 0 on signal.
pub async fn run(pool: PgPool, channels: &[String]) -> Result<()> {
    if channels.is_empty() {
        return Err(Error::InvalidArgument(
            "notify-stream requires at least one channel".into(),
        ));
    }

    let mut listener = PgListener::connect_with(&pool).await?;
    for ch in channels {
        listener.listen(ch).await?;
    }
    tracing::info!(channels = ?channels, "notify-stream started");

    let mut term = signal(SignalKind::terminate())
        .map_err(|e| Error::InvalidArgument(format!("install SIGTERM handler: {e}")))?;
    let mut intr = signal(SignalKind::interrupt())
        .map_err(|e| Error::InvalidArgument(format!("install SIGINT handler: {e}")))?;

    let stdout = std::io::stdout();
    loop {
        select! {
            ev = listener.recv() => {
                let n = ev?;
                let line = format!(
                    "{{\"channel\":{},\"payload\":{}}}\n",
                    serde_json::to_string(n.channel())?,
                    serde_json::to_string(n.payload())?,
                );
                let mut h = stdout.lock();
                h.write_all(line.as_bytes()).map_err(|e| io_to_err(&e))?;
                h.flush().map_err(|e| io_to_err(&e))?;
            }
            _ = term.recv() => { tracing::info!("SIGTERM"); break; }
            _ = intr.recv() => { tracing::info!("SIGINT"); break; }
        }
    }
    Ok(())
}

fn io_to_err(e: &std::io::Error) -> Error {
    Error::InvalidArgument(format!("stdout write: {e}"))
}
