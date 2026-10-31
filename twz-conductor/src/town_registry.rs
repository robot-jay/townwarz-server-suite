use std::{sync::Arc, time::{Duration, SystemTime, UNIX_EPOCH}};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::sync::Mutex;
use tokio_postgres::{Client, NoTls};
use twz_shared::{
    CreateTownRequest, CreateTownResponse, GameSession, Town, hall_for_slot,
    spawn_from_hall, validate_town_name, SUBJ_TOWN_CREATE, SUBJ_TOWN_LIST,
    SUBJ_GAME_END, SUBJ_GAME_TOUCH,
};

pub struct TownRegistry {
    nats: async_nats::Client,
    db: Mutex<Client>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use twz_shared::{ClientId, GameUser};

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL and TEST_NATS_URL"]
    async fn postgres_allocates_distinct_halls_and_never_resumes_ended_games() -> Result<()> {
        let nats = async_nats::connect(std::env::var("TEST_NATS_URL")?).await?;
        let registry = TownRegistry::new(nats, &std::env::var("TEST_DATABASE_URL")?).await?;
        let user = GameUser {
            sub: format!("integration|{}", ClientId::new_v4()),
            name: Some("Integration Mayor".into()), email: None,
        };
        let request = || CreateTownRequest { player_id: ClientId::new_v4(), desired_name: "  Copper Creek  ".into(), user: user.clone() };
        let (first, second) = tokio::join!(registry.create(request()), registry.create(request()));
        let (first, second) = (first?, second?);
        assert_ne!(first.hall.id, second.hall.id);
        assert_eq!(first.name, "Copper Creek");
        registry.end(&first.mayor_player_id.to_string()).await?;
        let third = registry.create(request()).await?;
        assert_ne!(first.id, third.id);
        assert_ne!(first.mayor_player_id, third.mayor_player_id);
        assert_eq!(first.hall.id, third.hall.id, "ended hall should be reusable");
        assert!(registry.create(CreateTownRequest {
            player_id: first.mayor_player_id, desired_name: "Cannot Resume".into(), user: user.clone(),
        }).await.is_err(), "reusing an ended game's id must fail");
        let db = registry.db.lock().await;
        let users: i64 = db.query_one("SELECT COUNT(*) FROM twz_users WHERE auth0_sub=$1", &[&user.sub]).await?.get(0);
        assert_eq!(users, 1, "one Auth0 identity, multiple fresh games");
        db.execute("UPDATE twz_games SET last_seen_at=NOW()-INTERVAL '60 seconds' WHERE auth0_sub=$1 AND ended_at IS NULL", &[&user.sub]).await?;
        drop(db);
        assert!(registry.expire_leases().await? >= 2);
        assert!(!registry.list().await?.iter().any(|town| town.id == third.id || town.id == second.id));
        let db = registry.db.lock().await;
        db.execute("DELETE FROM twz_games WHERE auth0_sub=$1", &[&user.sub]).await?;
        db.execute("DELETE FROM twz_users WHERE auth0_sub=$1", &[&user.sub]).await?;
        Ok(())
    }
}

impl TownRegistry {
    pub async fn new(nats: async_nats::Client, database_url: &str) -> Result<Self> {
        let (db, connection) = tokio_postgres::connect(database_url, NoTls).await
            .context("connect to TownWarz PostgreSQL database")?;
        tokio::spawn(async move {
            if let Err(error) = connection.await {
                tracing::error!("PostgreSQL connection failed: {error}");
            }
        });
        db.batch_execute(include_str!("../migrations/001_games.sql")).await?;
        Ok(Self { nats, db: Mutex::new(db) })
    }

    pub async fn serve(self) -> Result<()> {
        let mut creates = self.nats.subscribe(SUBJ_TOWN_CREATE).await?;
        let mut lists = self.nats.subscribe(SUBJ_TOWN_LIST).await?;
        let mut ends = self.nats.subscribe(SUBJ_GAME_END).await?;
        let mut touches = self.nats.subscribe(SUBJ_GAME_TOUCH).await?;
        let this = Arc::new(self);
        tokio::spawn(async move {
            let mut reap = tokio::time::interval(Duration::from_secs(10));
            loop {
                tokio::select! {
                    Some(message) = creates.next() => {
                        let result = match serde_json::from_slice::<CreateTownRequest>(&message.payload) {
                            Ok(request) => this.create(request).await,
                            Err(error) => Err(error.into()),
                        };
                        let response = match result {
                            Ok(town) => CreateTownResponse {
                                ok: true, error: None, spawn_tile: Some(spawn_from_hall(town.hall.origin)), town: Some(town),
                            },
                            Err(error) => {
                                tracing::warn!("town creation failed: {error:#}");
                                CreateTownResponse { ok: false, error: Some("Unable to create your town. Please try again.".into()), town: None, spawn_tile: None }
                            }
                        };
                        if let Some(reply) = message.reply {
                            let _ = this.nats.publish(reply, serde_json::to_vec(&response).unwrap().into()).await;
                        }
                    }
                    Some(message) = lists.next() => {
                        if let Some(reply) = message.reply {
                            match this.list().await {
                                Ok(towns) => { let _ = this.nats.publish(reply, serde_json::to_vec(&towns).unwrap().into()).await; }
                                Err(error) => tracing::warn!("town listing failed: {error}"),
                            }
                        }
                    }
                    Some(message) = ends.next() => {
                        if let Ok(session) = serde_json::from_slice::<GameSession>(&message.payload) {
                            if let Err(error) = this.end(&session.player_id.to_string()).await {
                                tracing::warn!("game cleanup failed: {error}");
                            }
                        }
                    }
                    Some(message) = touches.next() => {
                        if let Ok(session) = serde_json::from_slice::<GameSession>(&message.payload) {
                            let result = this.db.lock().await.execute(
                                "UPDATE twz_games SET last_seen_at = NOW() WHERE player_id = $1 AND ended_at IS NULL",
                                &[&session.player_id.to_string()],
                            ).await;
                            if let Err(error) = result { tracing::warn!("game heartbeat failed: {error}"); }
                        }
                    }
                    _ = reap.tick() => {
                        // Leases also release halls after a crashed gateway. Ended games cannot be revived.
                        let result = this.db.lock().await.execute(
                            "UPDATE twz_games SET ended_at = NOW() WHERE ended_at IS NULL AND last_seen_at < NOW() - INTERVAL '45 seconds'", &[],
                        ).await;
                        if let Err(error) = result { tracing::warn!("game lease cleanup failed: {error}"); }
                    }
                }
            }
        });
        Ok(())
    }

    async fn create(&self, request: CreateTownRequest) -> Result<Town> {
        let name = validate_town_name(&request.desired_name).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(!request.user.sub.is_empty(), "missing authenticated user");
        let mut db = self.db.lock().await;
        let tx = db.transaction().await?;
        // Serialize allocation across conductor instances, not just this process.
        tx.execute("SELECT pg_advisory_xact_lock(7489221)", &[]).await?;
        tx.execute(
            "INSERT INTO twz_users(auth0_sub, display_name, email) VALUES($1,$2,$3) ON CONFLICT(auth0_sub) DO UPDATE SET display_name=EXCLUDED.display_name, email=EXCLUDED.email, last_seen_at=NOW()",
            &[&request.user.sub, &request.user.name, &request.user.email],
        ).await?;
        let slot: i32 = tx.query_one(
            "SELECT n::integer FROM generate_series(0, (SELECT COUNT(*) FROM twz_games WHERE ended_at IS NULL)::integer) AS n WHERE NOT EXISTS (SELECT 1 FROM twz_games WHERE hall_slot = n AND ended_at IS NULL) ORDER BY n LIMIT 1",
            &[],
        ).await?.get(0);
        let town = Town {
            id: format!("t-{}", request.player_id), name, hall: hall_for_slot(slot),
            created_at_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64,
            mayor_player_id: request.player_id,
        };
        tx.execute(
            "INSERT INTO twz_games(player_id,auth0_sub,town_id,town_name,hall_slot,town_json) VALUES($1,$2,$3,$4,$5,$6::text::jsonb)",
            &[&request.player_id.to_string(), &request.user.sub, &town.id, &town.name, &slot, &serde_json::to_string(&town)?],
        ).await?;
        tx.commit().await?;
        Ok(town)
    }

    async fn list(&self) -> Result<Vec<Town>> {
        self.db.lock().await.query("SELECT town_json::text FROM twz_games WHERE ended_at IS NULL ORDER BY started_at", &[]).await?
            .into_iter().map(|row| Ok(serde_json::from_str(&row.get::<_, String>(0))?)).collect()
    }

    async fn end(&self, player_id: &str) -> Result<()> {
        self.db.lock().await.execute("UPDATE twz_games SET ended_at = NOW() WHERE player_id = $1 AND ended_at IS NULL", &[&player_id]).await?;
        Ok(())
    }

    #[cfg(test)]
    async fn expire_leases(&self) -> Result<u64> {
        Ok(self.db.lock().await.execute("UPDATE twz_games SET ended_at = NOW() WHERE ended_at IS NULL AND last_seen_at < NOW() - INTERVAL '45 seconds'", &[]).await?)
    }
}
