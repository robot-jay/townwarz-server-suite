use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::{
    entity::prelude::DateTimeWithTimeZone, sea_query::OnConflict, ActiveModelTrait, ColumnTrait,
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DatabaseTransaction, DbBackend,
    EntityTrait, FromQueryResult, QueryFilter, QueryOrder, QueryResult, QuerySelect, Set,
    Statement, TransactionTrait, Value,
};
use twz_shared::{
    hall_for_slot, ClientId, EndGameRequest, ExistingGameSummary, GameConnection, GameUser,
    PlayerState, Town, RECONNECT_GRACE_SECONDS,
};

use super::entities::{games, users};

const ALLOCATION_LOCK: i64 = 7_489_221;
const MIGRATION_LOCK: i64 = 7_489_220;
const STALE_CONNECTION_SECONDS: i64 = 45;

#[derive(FromQueryResult)]
struct GameSummaryRow {
    player_id: String,
    town_id: String,
    town_name: String,
    connection_id: Option<String>,
    disconnected_at: Option<DateTimeWithTimeZone>,
}

#[derive(Debug)]
pub(crate) struct GameAdmission {
    pub town: Town,
    pub connection_id: ClientId,
    pub player_state: PlayerState,
}

impl GameAdmission {
    fn from_model(model: games::Model) -> Result<Self> {
        let town: Town = serde_json::from_value(model.town_json)?;
        let player_state = model
            .checkpoint_json
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_else(|| PlayerState::initial_for_town(&town));
        let connection_id = model
            .connection_id
            .context("Missing game connection token.")?
            .parse()?;
        Ok(Self {
            town,
            connection_id,
            player_state,
        })
    }
}

/// The only application database boundary. Clones share a connection pool;
/// transactions/row locks, not a process-wide Mutex, protect concurrent claims.
#[derive(Clone)]
pub(crate) struct GameRepository {
    pub(super) db: DatabaseConnection,
}

impl GameRepository {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let mut options = ConnectOptions::new(database_url);
        options
            .max_connections(10)
            .min_connections(1)
            .connect_timeout(Duration::from_secs(8))
            .acquire_timeout(Duration::from_secs(8))
            .sqlx_logging(false);
        let db = Database::connect(options)
            .await
            .context("connect to TownWarz PostgreSQL database")?;
        anyhow::ensure!(
            db.get_database_backend() == DbBackend::Postgres,
            "TownWarz requires PostgreSQL."
        );

        // Preserve the existing idempotent schema files and serialize DDL when
        // several repository instances start against the same database.
        let migration = db.begin().await?;
        lock(&migration, MIGRATION_LOCK).await?;
        migration
            .execute_unprepared(include_str!("../../migrations/001_games.sql"))
            .await?;
        migration
            .execute_unprepared(include_str!("../../migrations/002_reconnect.sql"))
            .await?;
        migration.commit().await?;
        Ok(Self { db })
    }

    pub async fn create_game(
        &self,
        user: &GameUser,
        player_id: ClientId,
        name: &str,
    ) -> Result<GameAdmission> {
        let transaction = self.db.begin().await?;
        lock(&transaction, ALLOCATION_LOCK).await?;
        let existing = games::Entity::find()
            .select_only()
            .column(games::Column::PlayerId)
            .filter(games::Column::Auth0Sub.eq(&user.sub))
            .filter(games::Column::EndedAt.is_null())
            .into_tuple::<String>()
            .one(&transaction)
            .await?;
        anyhow::ensure!(existing.is_none(),
            "You already have a town. Reconnect to it or intentionally leave it before starting another game.");

        users::Entity::insert(users::ActiveModel {
            auth0_sub: Set(user.sub.clone()),
            display_name: Set(user.name.clone()),
            email: Set(user.email.clone()),
            ..Default::default()
        })
        .on_conflict(
            OnConflict::column(users::Column::Auth0Sub)
                .update_columns([users::Column::DisplayName, users::Column::Email])
                .value(users::Column::LastSeenAt, Expr::current_timestamp())
                .to_owned(),
        )
        .exec(&transaction)
        .await?;

        let slot = transaction
            .query_one(statement(include_str!("sql/allocate_hall.sql"), vec![]))
            .await?
            .context("No townhall slot was available.")?
            .try_get::<i32>("", "hall_slot")?;
        let previous = transaction
            .query_one(statement(
                include_str!("sql/latest_claim.sql"),
                vec![slot.into()],
            ))
            .await?
            .context("Missing townhall claim history.")?
            .try_get::<i64>("", "created_at_ms")?;
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
        let town = Town {
            id: format!("t-{player_id}"),
            name: name.to_owned(),
            hall: hall_for_slot(slot),
            created_at_ms: now_ms.max(previous.saturating_add(1)),
            mayor_player_id: player_id,
        };
        let model = games::ActiveModel {
            player_id: Set(player_id.to_string()),
            auth0_sub: Set(user.sub.clone()),
            role: Set("mayor".into()),
            town_id: Set(town.id.clone()),
            town_name: Set(town.name.clone()),
            hall_slot: Set(slot),
            town_json: Set(serde_json::to_value(&town)?),
            connection_id: Set(Some(ClientId::new_v4().to_string())),
            checkpoint_json: Set(Some(serde_json::to_value(PlayerState::initial_for_town(
                &town,
            ))?)),
            ..Default::default()
        }
        .insert(&transaction)
        .await?;
        let admission = GameAdmission::from_model(model)?;
        transaction.commit().await?;
        Ok(admission)
    }

    pub async fn resume_game(&self, subject: &str) -> Result<Option<GameAdmission>> {
        let transaction = self.db.begin().await?;
        lock(&transaction, ALLOCATION_LOCK).await?;
        let model = games::Entity::find()
            .from_raw_sql(statement(
                include_str!("sql/resume_game.sql"),
                vec![subject.into(), (RECONNECT_GRACE_SECONDS as i64).into()],
            ))
            .one(&transaction)
            .await?;
        let Some(mut model) = model else {
            transaction.commit().await?;
            return Ok(None);
        };
        anyhow::ensure!(model.connection_id.is_none(),
            "Your game is still connected elsewhere, or its connection is still closing. Try reconnecting shortly.");

        let connection_id = ClientId::new_v4().to_string();
        games::Entity::update_many()
            .col_expr(
                games::Column::ConnectionId,
                Expr::value(connection_id.clone()),
            )
            .col_expr(
                games::Column::DisconnectedAt,
                Expr::value(None::<DateTimeWithTimeZone>),
            )
            .col_expr(games::Column::LastSeenAt, Expr::current_timestamp().into())
            .filter(games::Column::PlayerId.eq(&model.player_id))
            .exec(&transaction)
            .await?;
        model.connection_id = Some(connection_id);
        model.disconnected_at = None;
        // Build from the locked row, not a separate post-commit read that could
        // observe another transition or lose the token we just claimed.
        let admission = GameAdmission::from_model(model)?;
        transaction.commit().await?;
        Ok(Some(admission))
    }

    pub async fn active_towns(&self) -> Result<Vec<Town>> {
        // Directory polling needs labels/locations, not every player's private
        // checkpoint and timestamps. Keep the ORM projection as small as before.
        games::Entity::find()
            .select_only()
            .column(games::Column::TownJson)
            .filter(games::Column::EndedAt.is_null())
            .order_by_asc(games::Column::StartedAt)
            .into_tuple::<serde_json::Value>()
            .all(&self.db)
            .await?
            .into_iter()
            .map(|town| Ok(serde_json::from_value(town)?))
            .collect()
    }

    /// Read-only entry lookup: no connection claim, checkpoint load or simulation attachment.
    pub async fn active_game_summary(&self, subject: &str) -> Result<Option<ExistingGameSummary>> {
        let row = games::Entity::find()
            .select_only()
            .columns([
                games::Column::PlayerId,
                games::Column::TownId,
                games::Column::TownName,
                games::Column::ConnectionId,
                games::Column::DisconnectedAt,
            ])
            .filter(games::Column::Auth0Sub.eq(subject))
            .filter(games::Column::EndedAt.is_null())
            .order_by_desc(games::Column::StartedAt)
            .into_model::<GameSummaryRow>()
            .one(&self.db)
            .await?;
        row.map(|row| {
            Ok(ExistingGameSummary {
                player_id: row.player_id.parse()?,
                town_id: row.town_id,
                town_name: row.town_name,
                is_connected: row.connection_id.is_some(),
                reconnect_until_ms: row
                    .disconnected_at
                    .map(|at| at.timestamp_millis() + RECONNECT_GRACE_SECONDS as i64 * 1000),
            })
        })
        .transpose()
    }

    pub async fn active_player_ids(&self, subject: &str) -> Result<Vec<ClientId>> {
        games::Entity::find()
            .select_only()
            .column(games::Column::PlayerId)
            .filter(games::Column::Auth0Sub.eq(subject))
            .filter(games::Column::EndedAt.is_null())
            .into_tuple::<String>()
            .all(&self.db)
            .await?
            .into_iter()
            .map(|id| Ok(id.parse()?))
            .collect()
    }

    pub async fn update_connection(
        &self,
        request: &GameConnection,
        disconnected: bool,
    ) -> Result<bool> {
        if let Some(state) = &request.checkpoint {
            anyhow::ensure!(
                state.id == request.player_id && state.game.identity.game_id == request.player_id,
                "Checkpoint identity does not match the game."
            );
            anyhow::ensure!(
                [state.x, state.y, state.vx, state.vy, state.ang]
                    .iter()
                    .all(|v| v.is_finite()),
                "Invalid checkpoint coordinates."
            );
        }
        let checkpoint = request
            .checkpoint
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?;
        let sql = if disconnected {
            include_str!("sql/disconnect_connection.sql")
        } else {
            include_str!("sql/touch_connection.sql")
        };
        let result = self
            .db
            .execute(statement(
                sql,
                vec![
                    request.player_id.to_string().into(),
                    request.connection_id.to_string().into(),
                    checkpoint.into(),
                ],
            ))
            .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn end_game(&self, request: &EndGameRequest) -> Result<Option<Town>> {
        let reason = serde_json::to_value(request.reason)?
            .as_str()
            .context("Invalid game end reason.")?
            .to_owned();
        let row = self
            .db
            .query_one(statement(
                include_str!("sql/end_game.sql"),
                vec![
                    request.player_id.to_string().into(),
                    request.connection_id.map(|id| id.to_string()).into(),
                    reason.into(),
                ],
            ))
            .await?;
        row.map(town_from_row).transpose()
    }

    pub async fn expire_games(&self) -> Result<Vec<Town>> {
        let transaction = self.db.begin().await?;
        lock(&transaction, ALLOCATION_LOCK).await?;
        transaction
            .execute(statement(
                include_str!("sql/disconnect_stale.sql"),
                vec![STALE_CONNECTION_SECONDS.into()],
            ))
            .await?;
        let rows = transaction
            .query_all(statement(
                include_str!("sql/expire_games.sql"),
                vec![(RECONNECT_GRACE_SECONDS as i64).into()],
            ))
            .await?;
        let towns = rows
            .into_iter()
            .map(town_from_row)
            .collect::<Result<Vec<Town>>>()?;
        transaction.commit().await?;
        Ok(towns)
    }
}

fn town_from_row(row: QueryResult) -> Result<Town> {
    Ok(serde_json::from_value(
        row.try_get::<serde_json::Value>("", "town_json")?,
    )?)
}

fn statement(sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

async fn lock(transaction: &DatabaseTransaction, key: i64) -> Result<()> {
    transaction
        .query_one(statement(
            "SELECT pg_advisory_xact_lock($1)",
            vec![key.into()],
        ))
        .await?;
    Ok(())
}
