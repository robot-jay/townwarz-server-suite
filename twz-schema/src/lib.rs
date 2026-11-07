//! Read-only application/schema compatibility contract. No migrator or DDL is
//! linked here; runtime services can use a database identity without DDL rights.
use anyhow::{Context, Result};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use std::collections::{HashMap, HashSet};

pub const MIGRATION_LOCK: i64 = 7_489_220;
pub const ALLOCATION_LOCK: i64 = 7_489_221;
pub const READER_VERSION: i32 = 1;
pub const REQUIRED_SCHEMA_VERSION: i32 = 6;
pub const MIGRATION_NAMES: [&str; 6] = [
    "m0001_games",
    "m0002_reconnect",
    "m0003_expiry_notice",
    "m0004_economy",
    "m0005_participation_order",
    "m0006_schema_contract",
];

#[derive(Debug, PartialEq, Eq)]
pub struct SchemaInfo {
    pub version: i32,
    pub minimum_reader_version: i32,
}

/// Newer additive schemas remain readable while their minimum reader contract
/// supports this binary. A breaking migration must raise that minimum.
pub async fn ensure_compatible(db: &impl ConnectionTrait) -> Result<SchemaInfo> {
    anyhow::ensure!(
        db.get_database_backend() == DbBackend::Postgres,
        "TownWarz requires PostgreSQL"
    );
    let row = db.query_one(Statement::from_string(DbBackend::Postgres,
        "SELECT schema_version, minimum_reader_version FROM twz_schema_metadata WHERE id=1".to_owned()))
        .await.context("Schema metadata is unavailable. Run `twz-migrate up` as a deployment step, then grant the conductor SELECT on schema/migration metadata")?
        .context("Schema metadata is missing. Run `twz-migrate up` before starting the conductor")?;
    let info = SchemaInfo {
        version: row.try_get("", "schema_version")?,
        minimum_reader_version: row.try_get("", "minimum_reader_version")?,
    };
    anyhow::ensure!(info.version >= REQUIRED_SCHEMA_VERSION,
        "Database schema {} is older than required schema {}. Run `twz-migrate up` before starting this release", info.version, REQUIRED_SCHEMA_VERSION);
    anyhow::ensure!(info.minimum_reader_version > 0 && info.minimum_reader_version <= READER_VERSION,
        "Database requires schema reader {}, but this binary supports reader {}. Deploy a compatible application release; migrations are forward-only", info.minimum_reader_version, READER_VERSION);
    let history = db.query_all(Statement::from_string(DbBackend::Postgres,
        "SELECT version FROM seaql_migrations".to_owned())).await
        .context("Cannot read migration history. Run deployment migrations and grant runtime SELECT on seaql_migrations")?;
    let applied = history
        .into_iter()
        .map(|row| row.try_get::<String>("", "version"))
        .collect::<Result<HashSet<_>, _>>()?;
    anyhow::ensure!(MIGRATION_NAMES.iter().all(|name| applied.contains(*name)) && applied.len() == info.version as usize,
        "Schema contract and migration history do not match. Run the release's `twz-migrate up`; do not manually stamp migration versions");
    validate_shape(db).await?;
    Ok(info)
}

/// Verify real columns/types, not just the presence of a version stamp. Legacy
/// databases are adopted by running the SQL, never by blind history baselining.
pub async fn validate_shape(db: &impl ConnectionTrait) -> Result<()> {
    let columns = db.query_all(Statement::from_string(DbBackend::Postgres,
        "SELECT table_name, column_name, udt_name FROM information_schema.columns WHERE table_schema=current_schema() AND table_name IN ('twz_users','twz_games')".to_owned())).await?;
    let mut actual = HashMap::new();
    for row in columns {
        actual.insert(
            (
                row.try_get::<String>("", "table_name")?,
                row.try_get::<String>("", "column_name")?,
            ),
            row.try_get::<String>("", "udt_name")?,
        );
    }
    let users = [
        ("auth0_sub", "text"),
        ("display_name", "text"),
        ("email", "text"),
        ("created_at", "timestamptz"),
        ("last_seen_at", "timestamptz"),
    ];
    let games = [
        ("player_id", "text"),
        ("auth0_sub", "text"),
        ("role", "text"),
        ("town_id", "text"),
        ("town_name", "text"),
        ("hall_slot", "int4"),
        ("town_json", "jsonb"),
        ("started_at", "timestamptz"),
        ("last_seen_at", "timestamptz"),
        ("ended_at", "timestamptz"),
        ("connection_id", "text"),
        ("disconnected_at", "timestamptz"),
        ("checkpoint_json", "jsonb"),
        ("end_reason", "text"),
        ("expiry_notice_ack_at", "timestamptz"),
        ("economy_json", "jsonb"),
        ("participation_order", "int8"),
    ];
    for (table, expected) in [
        ("twz_users", users.as_slice()),
        ("twz_games", games.as_slice()),
    ] {
        for (column, ty) in expected {
            anyhow::ensure!(actual.get(&(table.to_owned(), (*column).to_owned())).map(String::as_str) == Some(*ty),
                "Incompatible schema: expected {table}.{column} of PostgreSQL type {ty}. Apply deployment migrations or repair schema drift before startup");
        }
    }
    Ok(())
}
