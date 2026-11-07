//! Versioned, forward-only deployment migrations. Runtime applications depend
//! only on twz-schema, not on this runner or its DDL implementation.
use anyhow::Result;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use sea_orm_migration::prelude::*;

struct Migrator;
struct SqlMigration {
    name: &'static str,
    sql: &'static str,
}
impl MigrationName for SqlMigration {
    fn name(&self) -> &str {
        self.name
    }
}
#[async_trait::async_trait]
impl MigrationTrait for SqlMigration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(self.sql)
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration("TownWarz migrations are forward-only; deploy a compatible application or an explicit corrective migration".into()))
    }
}
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        let scripts = [
            include_str!("../migrations/001_games.sql"),
            include_str!("../migrations/002_reconnect.sql"),
            include_str!("../migrations/003_expiry_notice.sql"),
            include_str!("../migrations/004_economy.sql"),
            include_str!("../migrations/005_participation_order.sql"),
            include_str!("../migrations/006_schema_contract.sql"),
        ];
        twz_schema::MIGRATION_NAMES
            .into_iter()
            .zip(scripts)
            .map(|(name, sql)| Box::new(SqlMigration { name, sql }) as Box<dyn MigrationTrait>)
            .collect()
    }
}

/// One atomic release operation. Lock before creating the migration ledger, and
/// retain the allocation lock while legacy participation sequences are adopted.
pub async fn migrate(db: &DatabaseConnection) -> Result<twz_schema::SchemaInfo> {
    anyhow::ensure!(
        db.get_database_backend() == DbBackend::Postgres,
        "TownWarz requires PostgreSQL"
    );
    let transaction = db.begin().await?;
    for lock in [twz_schema::MIGRATION_LOCK, twz_schema::ALLOCATION_LOCK] {
        transaction
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT pg_advisory_xact_lock($1)",
                vec![lock.into()],
            ))
            .await?;
    }
    Migrator::up(&transaction, None).await?;
    let schema = twz_schema::ensure_compatible(&transaction).await?;
    transaction.commit().await?;
    Ok(schema)
}
