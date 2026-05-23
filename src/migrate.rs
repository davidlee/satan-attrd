//! Schema migration runner.
//!
//! Migrations are embedded at compile time from `./migrations/`. Runs
//! explicitly via the `migrate` subcommand — the daemon never auto-migrates
//! on start (would race other agents touching the same database).

use sqlx::PgPool;

/// Apply any pending migrations.
///
/// # Errors
///
/// Returns an error if any migration step fails to apply.
pub async fn run_migrations(pool: &PgPool) -> crate::error::Result<()> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(|e| crate::error::Error::Migration(e.to_string()))?;
    Ok(())
}
