use std::{sync::Arc, time::Duration};

use anyhow::Result;
use futures_util::StreamExt;
use twz_shared::*;

use crate::persistence::{GameAdmission, GameRepository};

/// Application service: validates commands, dispatches NATS requests and emits
/// lifecycle events. Database entities, connections and SQL belong to persistence.
pub struct TownRegistry {
    nats: async_nats::Client,
    games: GameRepository,
}

impl TownRegistry {
    pub async fn new(nats: async_nats::Client, database_url: &str) -> Result<Self> {
        Ok(Self {
            nats,
            games: GameRepository::connect(database_url).await?,
        })
    }

    pub async fn serve(self) -> Result<()> {
        let mut creates = self.nats.subscribe(SUBJ_TOWN_CREATE).await?;
        let mut resumes = self.nats.subscribe(SUBJ_GAME_RESUME).await?;
        let mut checks = self.nats.subscribe(SUBJ_GAME_CHECK).await?;
        let mut expiry_acks = self.nats.subscribe(SUBJ_EXPIRY_ACK).await?;
        let mut quits = self.nats.subscribe(SUBJ_GAME_QUIT_USER).await?;
        let mut lists = self.nats.subscribe(SUBJ_TOWN_LIST).await?;
        let mut ends = self.nats.subscribe(SUBJ_GAME_END).await?;
        let mut touches = self.nats.subscribe(SUBJ_GAME_TOUCH).await?;
        let mut disconnects = self.nats.subscribe(SUBJ_GAME_DISCONNECT).await?;
        let this = Arc::new(self);
        tokio::spawn(async move {
            let mut reap = tokio::time::interval(Duration::from_secs(10));
            loop {
                let (message, response) = tokio::select! {
                    Some(message) = creates.next() => {
                        let result = async {
                            this.expire_leases().await?;
                            let request: CreateTownRequest = serde_json::from_slice(&message.payload)?;
                            this.create(request).await
                        }.await;
                        (message, Self::entry_response(result))
                    }
                    Some(message) = resumes.next() => {
                        let result = async {
                            this.expire_leases().await?;
                            let request: ResumeGameRequest = serde_json::from_slice(&message.payload)?;
                            let admission = this.games.resume_game(&request.user.sub).await?;
                            Ok(Self::admission_response(admission))
                        }.await;
                        (message, Self::entry_response(result))
                    }
                    Some(message) = checks.next() => {
                        let result = async {
                            this.expire_leases().await?;
                            let request: ResumeGameRequest = serde_json::from_slice(&message.payload)?;
                            let game = this.games.active_game_summary(&request.user.sub).await?;
                            let expired_game = if game.is_none() { this.games.expired_game_notice(&request.user.sub).await? } else { None };
                            Ok::<_, anyhow::Error>((game, expired_game))
                        }.await;
                        let response = match result {
                            Ok((game, expired_game)) => GameLookupResponse { ok: true, error: None, game, expired_game },
                            Err(error) => GameLookupResponse { ok: false, error: Some(error.to_string()), game: None, expired_game: None },
                        };
                        (message, serde_json::to_vec(&response).unwrap())
                    }
                    Some(message) = expiry_acks.next() => {
                        let result = async {
                            let request: AcknowledgeExpiryRequest = serde_json::from_slice(&message.payload)?;
                            this.games.acknowledge_expiry(&request.user.sub, request.player_id).await
                        }.await;
                        (message, Self::operation_response(result))
                    }
                    Some(message) = lists.next() => {
                        let response = match this.games.active_towns().await {
                            Ok(towns) => serde_json::to_vec(&towns).unwrap(),
                            Err(error) => {
                                tracing::warn!("town listing failed: {error}");
                                continue;
                            }
                        };
                        (message, response)
                    }
                    Some(message) = ends.next() => {
                        let result = async {
                            let request: EndGameRequest = serde_json::from_slice(&message.payload)?;
                            this.end_game(request).await
                        }.await;
                        (message, Self::operation_response(result))
                    }
                    Some(message) = quits.next() => {
                        let result = async {
                            let request: QuitUserGameRequest = serde_json::from_slice(&message.payload)?;
                            for player_id in this.games.active_player_ids(&request.user.sub).await? {
                                if request.player_id.is_some_and(|expected| expected != player_id) { continue; }
                                this.end_game(EndGameRequest {
                                    player_id, connection_id: None, reason: GameEndReason::IntentionalQuit,
                                }).await?;
                            }
                            Ok(())
                        }.await;
                        (message, Self::operation_response(result))
                    }
                    Some(message) = touches.next() => {
                        let result = async {
                            let request: GameConnection = serde_json::from_slice(&message.payload)?;
                            this.connection_update(request, false).await
                        }.await;
                        (message, Self::operation_response(result))
                    }
                    Some(message) = disconnects.next() => {
                        let result = async {
                            let request: GameConnection = serde_json::from_slice(&message.payload)?;
                            this.connection_update(request, true).await
                        }.await;
                        (message, Self::operation_response(result))
                    }
                    _ = reap.tick() => {
                        if let Err(error) = this.expire_leases().await {
                            tracing::warn!("game expiry failed: {error}");
                        }
                        continue;
                    }
                };
                if let Some(reply) = message.reply {
                    let _ = this.nats.publish(reply, response.into()).await;
                }
            }
        });
        Ok(())
    }

    async fn create(&self, request: CreateTownRequest) -> Result<CreateTownResponse> {
        let name = validate_town_name(&request.desired_name).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(!request.user.sub.is_empty(), "Missing authenticated user.");
        let admission = self
            .games
            .create_game(&request.user, request.player_id, &name)
            .await?;
        Ok(Self::admission_response(Some(admission)))
    }

    fn admission_response(admission: Option<GameAdmission>) -> CreateTownResponse {
        match admission {
            Some(admission) => CreateTownResponse {
                ok: true,
                error: None,
                spawn_tile: Some(spawn_from_hall(admission.town.hall.origin)),
                town: Some(admission.town),
                connection_id: Some(admission.connection_id),
                player_state: Some(admission.player_state),
            },
            None => CreateTownResponse {
                ok: true,
                error: None,
                town: None,
                spawn_tile: None,
                connection_id: None,
                player_state: None,
            },
        }
    }

    fn entry_response(result: Result<CreateTownResponse>) -> Vec<u8> {
        let response = result.unwrap_or_else(|error| {
            tracing::warn!("game entry failed: {error:#}");
            CreateTownResponse {
                ok: false,
                error: Some(error.to_string()),
                town: None,
                spawn_tile: None,
                connection_id: None,
                player_state: None,
            }
        });
        serde_json::to_vec(&response).unwrap()
    }

    fn operation_response(result: Result<()>) -> Vec<u8> {
        match result {
            Ok(()) => br#"{"ok":true}"#.to_vec(),
            Err(error) => {
                tracing::warn!("game operation failed: {error:#}");
                serde_json::json!({"ok":false,"error":error.to_string()})
                    .to_string()
                    .into_bytes()
            }
        }
    }

    async fn connection_update(&self, request: GameConnection, disconnected: bool) -> Result<()> {
        anyhow::ensure!(
            self.games.update_connection(&request, disconnected).await?,
            "This game connection is no longer active."
        );
        Ok(())
    }

    async fn end_game(&self, request: EndGameRequest) -> Result<()> {
        if let Some(town) = self.games.end_game(&request).await? {
            self.publish_ended(town, request.reason).await?;
        }
        Ok(())
    }

    async fn publish_ended(&self, town: Town, reason: GameEndReason) -> Result<()> {
        // No successor roles are playable yet. Future promotion handling must
        // replace this no-successor outcome when eligible members exist.
        if matches!(
            reason,
            GameEndReason::IntentionalQuit | GameEndReason::Timeout
        ) {
            self.nats
                .publish(
                    SUBJ_PROMOTION_REQUESTED,
                    serde_json::json!({
                        "town":town, "former_mayor_id":town.mayor_player_id, "reason":reason,
                        "eligible_successors":[], "outcome":"no_successors",
                    })
                    .to_string()
                    .into(),
                )
                .await?;
        }
        self.nats
            .publish(
                SUBJ_GAME_ENDED,
                serde_json::to_vec(&GameEnded {
                    player_id: town.mayor_player_id,
                    reason,
                })?
                .into(),
            )
            .await?;
        Ok(())
    }

    async fn expire_leases(&self) -> Result<u64> {
        let towns = self.games.expire_games().await?;
        let count = towns.len() as u64;
        for town in towns {
            self.publish_ended(town, GameEndReason::Timeout).await?;
        }
        Ok(count)
    }
}
