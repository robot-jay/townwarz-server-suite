use anyhow::{Context, Result};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use std::process::Command;
use uuid::Uuid;

struct Fixture {
    admin: DatabaseConnection,
    db: DatabaseConnection,
    schema: String,
    url: String,
}
impl Fixture {
    async fn new() -> Result<Self> {
        let base = std::env::var("TEST_DATABASE_URL").context("use an isolated test database")?;
        let admin = Database::connect(&base).await?;
        let schema = format!("migration_test_{}", Uuid::new_v4().simple());
        admin
            .execute_unprepared(&format!("CREATE SCHEMA {schema}"))
            .await?;
        let mut url = url::Url::parse(&base)?;
        url.query_pairs_mut()
            .append_pair("options", &format!("-c search_path={schema}"));
        let url = url.to_string();
        let db = Database::connect(&url).await?;
        Ok(Self {
            admin,
            db,
            schema,
            url,
        })
    }
    async fn cleanup(self) -> Result<()> {
        self.db.close().await?;
        self.admin
            .execute_unprepared(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .await?;
        self.admin.close().await?;
        Ok(())
    }
    async fn history(&self) -> Result<Vec<(String, i64)>> {
        Ok(self
            .db
            .query_all(Statement::from_string(
                DbBackend::Postgres,
                "SELECT version, applied_at FROM seaql_migrations ORDER BY version".to_owned(),
            ))
            .await?
            .into_iter()
            .map(|row| Ok((row.try_get("", "version")?, row.try_get("", "applied_at")?)))
            .collect::<Result<_>>()?)
    }
    fn cli(&self, command: &str) -> Result<std::process::Output> {
        Ok(Command::new(env!("CARGO_BIN_EXE_twz-migrate"))
            .arg(command)
            .env("DATABASE_URL", &self.url)
            .output()?)
    }
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL with permission to create test schemas"]
async fn postgres_migrations_empty_database_cli_gate_and_idempotent_history() -> Result<()> {
    let fixture = Fixture::new().await?;
    let check = fixture.cli("check")?;
    assert!(
        !check.status.success(),
        "read-only check must not initialize an empty database"
    );
    let count = fixture.db.query_one(Statement::from_string(DbBackend::Postgres, "SELECT COUNT(*)::bigint AS count FROM information_schema.tables WHERE table_schema=current_schema()".to_owned())).await?.unwrap();
    assert_eq!(count.try_get::<i64>("", "count")?, 0);
    let up = fixture.cli("up")?;
    assert!(
        up.status.success(),
        "{}",
        String::from_utf8_lossy(&up.stderr)
    );
    assert!(fixture.cli("check")?.status.success());
    let history = fixture.history().await?;
    assert_eq!(history.len(), twz_schema::MIGRATION_NAMES.len());
    assert!(fixture.cli("up")?.status.success());
    assert_eq!(
        history,
        fixture.history().await?,
        "repeated deployment does not rerun/stamp applied migrations"
    );
    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL with permission to create test schemas"]
async fn postgres_migrations_adopt_legacy_profiles_checkpoints_and_economy_without_data_loss(
) -> Result<()> {
    let fixture = Fixture::new().await?;
    // Exactly the SQL that the old conductor used, with no migration ledger.
    for script in [
        include_str!("../migrations/001_games.sql"),
        include_str!("../migrations/002_reconnect.sql"),
        include_str!("../migrations/003_expiry_notice.sql"),
        include_str!("../migrations/004_economy.sql"),
        include_str!("../migrations/005_participation_order.sql"),
    ] {
        fixture.db.execute_unprepared(script).await?;
    }
    let id = Uuid::new_v4().to_string();
    let checkpoint = serde_json::json!({"game":{"wallet":{"cash":125000},"inventory":{"slots":[{"item_type":"wall"},null,null,null,null,null]}},"x":816,"y":912});
    let economy = serde_json::json!({"cash":120000,"revision":5,"buildings":[{"price_paid":5000}],"outcomes":{"known-request":{"cash":120000}}});
    fixture.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO twz_users(auth0_sub, display_name, email) VALUES('legacy-account','Legacy Mayor','legacy@example.test')", vec![])).await?;
    fixture.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO twz_games(player_id,auth0_sub,town_id,town_name,hall_slot,town_json,checkpoint_json,economy_json,ended_at,end_reason,expiry_notice_ack_at) VALUES($1,'legacy-account','legacy-town','Legacy Town',0,$2,$3,$4,NOW(),'timeout',NOW())",
        vec![id.clone().into(), serde_json::json!({"id":"legacy-town","created_at_ms":123}).into(), checkpoint.clone().into(), economy.clone().into()])).await?;
    let before = fixture
        .db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT participation_order FROM twz_games".to_owned(),
        ))
        .await?
        .unwrap()
        .try_get::<i64>("", "participation_order")?;
    twz_migrate::migrate(&fixture.db).await?;
    let row = fixture
        .db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT g.*,u.display_name,u.email FROM twz_games g JOIN twz_users u USING(auth0_sub)"
                .to_owned(),
        ))
        .await?
        .unwrap();
    assert_eq!(row.try_get::<String>("", "player_id")?, id);
    assert_eq!(row.try_get::<String>("", "display_name")?, "Legacy Mayor");
    assert_eq!(row.try_get::<String>("", "email")?, "legacy@example.test");
    assert_eq!(
        row.try_get::<serde_json::Value>("", "checkpoint_json")?,
        checkpoint
    );
    assert_eq!(
        row.try_get::<serde_json::Value>("", "economy_json")?,
        economy
    );
    assert_eq!(row.try_get::<String>("", "end_reason")?, "timeout");
    assert_eq!(row.try_get::<i64>("", "participation_order")?, before);
    let acknowledgement = fixture
        .db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT expiry_notice_ack_at IS NOT NULL AS retained FROM twz_games".to_owned(),
        ))
        .await?
        .unwrap();
    assert!(acknowledgement.try_get::<bool>("", "retained")?);
    assert_eq!(fixture.history().await?.len(), 6);
    fixture.cleanup().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated TEST_DATABASE_URL with permission to create test schemas"]
async fn postgres_migrations_concurrent_runners_serialize_ledger_creation_and_ddl() -> Result<()> {
    let fixture = Fixture::new().await?;
    let second = Database::connect(&fixture.url).await?;
    let (a, b) = tokio::join!(
        twz_migrate::migrate(&fixture.db),
        twz_migrate::migrate(&second)
    );
    assert_eq!(a?.version, 6);
    assert_eq!(b?.version, 6);
    assert_eq!(fixture.history().await?.len(), 6);
    second.close().await?;
    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL with permission to create test schemas"]
async fn postgres_migrations_reject_schema_drift_and_roll_back_failed_release() -> Result<()> {
    let fixture = Fixture::new().await?;
    fixture
        .db
        .execute_unprepared(include_str!("../migrations/001_games.sql"))
        .await?;
    fixture
        .db
        .execute_unprepared("ALTER TABLE twz_games ALTER COLUMN town_name TYPE VARCHAR(32)")
        .await?;
    let error = twz_migrate::migrate(&fixture.db).await.unwrap_err();
    assert!(format!("{error:#}").contains("twz_games.town_name"));
    let row = fixture.db.query_one(Statement::from_string(DbBackend::Postgres,
        "SELECT to_regclass('seaql_migrations')::text AS ledger, to_regclass('twz_schema_metadata')::text AS metadata".to_owned())).await?.unwrap();
    assert!(row.try_get::<Option<String>>("", "ledger")?.is_none());
    assert!(row.try_get::<Option<String>>("", "metadata")?.is_none());
    let columns = fixture.db.query_one(Statement::from_string(DbBackend::Postgres,
        "SELECT COUNT(*)::bigint AS count FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='twz_games' AND column_name='checkpoint_json'".to_owned())).await?.unwrap();
    assert_eq!(
        columns.try_get::<i64>("", "count")?,
        0,
        "failed release rolled back its column additions too"
    );
    fixture.cleanup().await
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL with permission to create test schemas"]
async fn postgres_migrations_reader_contract_accepts_additive_and_rejects_breaking_future_schema(
) -> Result<()> {
    let fixture = Fixture::new().await?;
    twz_migrate::migrate(&fixture.db).await?;
    fixture.db.execute_unprepared("INSERT INTO seaql_migrations(version,applied_at) VALUES('m0007_additive_fixture',0); UPDATE twz_schema_metadata SET schema_version=7").await?;
    assert_eq!(twz_schema::ensure_compatible(&fixture.db).await?.version, 7);
    fixture
        .db
        .execute_unprepared("UPDATE twz_schema_metadata SET minimum_reader_version=2")
        .await?;
    assert!(format!(
        "{:#}",
        twz_schema::ensure_compatible(&fixture.db)
            .await
            .unwrap_err()
    )
    .contains("requires schema reader 2"));
    assert!(
        twz_migrate::migrate(&fixture.db).await.is_err(),
        "an older runner must not downgrade or adopt unknown migrations"
    );
    fixture.cleanup().await
}
