//! Postgres pool lifecycle.

#[expect(
    clippy::pub_use,
    clippy::module_name_repetitions,
    reason = "crate facade re-export; idiomatic name"
)]
pub use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// Create a connection pool sized for daemon workload.
///
/// `max_connections=8`: daemon holds a steady set of connections for the
/// `LISTENer` + dispatcher + rebuild driver; raise if observed contention.
///
/// # Errors
///
/// Returns an error if the database is unreachable or rejects the connect.
#[expect(clippy::module_name_repetitions, reason = "idiomatic factory name")]
pub async fn create_pool(database_url: &str) -> crate::error::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(database_url)
        .await?;
    Ok(pool)
}
