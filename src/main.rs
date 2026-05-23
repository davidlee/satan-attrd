//! satan-attrd — SATAN attribute layer daemon.
//!
//! T-attr-1b implements two subcommands:
//!
//! ```text
//! satan-attrd migrate    — run pending schema migrations and exit
//! satan-attrd rebuild    — replay event log into the projection
//!                          (`--include-disabled` to include disabled events)
//! ```
//!
//! `satan-attrd run` (the LISTENer / dispatcher loop) lands with T-attr-1c.

use std::process::ExitCode;

use tracing_subscriber::{EnvFilter, fmt};

use satan_attrd::{migrate, pool, store};

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}

fn database_url() -> Result<String, String> {
    std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is not set".to_string())
}

#[allow(clippy::print_stderr)]
fn print_usage() {
    eprintln!(
        "satan-attrd {ver}\n\
         \n\
         usage:\n  satan-attrd migrate\n  satan-attrd rebuild [--include-disabled]\n",
        ver = satan_attrd::VERSION,
    );
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    init_tracing();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        print_usage();
        return ExitCode::from(2);
    };

    let url = match database_url() {
        Ok(u) => u,
        Err(e) => {
            tracing::error!("{e}");
            return ExitCode::from(2);
        }
    };

    match cmd {
        "migrate" => match run_migrate(&url).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!(?e, "migrate failed");
                ExitCode::FAILURE
            }
        },
        "rebuild" => {
            let include_disabled = args.iter().any(|a| a == "--include-disabled");
            match run_rebuild(&url, include_disabled).await {
                Ok(n) => {
                    tracing::info!(events = n, "rebuild complete");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    tracing::error!(?e, "rebuild failed");
                    ExitCode::FAILURE
                }
            }
        }
        other => {
            tracing::error!(cmd = other, "unknown subcommand");
            print_usage();
            ExitCode::from(2)
        }
    }
}

async fn run_migrate(url: &str) -> satan_attrd::Result<()> {
    let pool = pool::create_pool(url).await?;
    migrate::run_migrations(&pool).await?;
    tracing::info!("migrations applied");
    Ok(())
}

async fn run_rebuild(url: &str, include_disabled: bool) -> satan_attrd::Result<usize> {
    let pool = pool::create_pool(url).await?;
    store::rebuild_projection(&pool, include_disabled).await
}
