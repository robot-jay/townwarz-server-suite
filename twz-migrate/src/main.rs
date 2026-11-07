use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sea_orm::{ConnectOptions, Database};
use std::time::Duration;

#[derive(Parser)]
#[command(about = "TownWarz deployment-time database migrations (forward-only)")]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Apply pending release migrations once, then exit. Requires deployment DDL privileges.
    Up,
    /// Read-only check that this application's schema contract is supported.
    Check,
}
#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args = Args::parse();
    let url = std::env::var("DATABASE_URL")
        .context("Set DATABASE_URL for the migration/deployment database identity")?;
    let mut options = ConnectOptions::new(url);
    options
        .max_connections(1)
        .min_connections(1)
        .connect_timeout(Duration::from_secs(8))
        .acquire_timeout(Duration::from_secs(8))
        .sqlx_logging(false);
    let db = Database::connect(options)
        .await
        .context("connect to deployment database")?;
    let schema = match args.command {
        Command::Up => twz_migrate::migrate(&db)
            .await
            .context("deployment migration failed; application rollout must not continue")?,
        Command::Check => twz_schema::ensure_compatible(&db).await?,
    };
    println!(
        "Schema {} is compatible with reader {} (minimum reader {})",
        schema.version,
        twz_schema::READER_VERSION,
        schema.minimum_reader_version
    );
    db.close().await?;
    Ok(())
}
