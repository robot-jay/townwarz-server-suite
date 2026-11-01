use super::*;
use twz_shared::{
    ExistingGameSummary, GameLookupResponse, QuitUserGameRequest, ResumeGameRequest, CELL_TILES,
    SUBJ_GAME_CHECK, SUBJ_GAME_DISCONNECT, SUBJ_GAME_QUIT_USER, SUBJ_GAME_RESUME, TILE_PX,
};

pub(super) async fn operation<T: serde::Serialize>(
    nats: &async_nats::Client,
    subject: &str,
    request: &T,
) -> Result<()> {
    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        nats.request(subject.to_owned(), serde_json::to_vec(request)?.into()),
    )
    .await??;
    let value: serde_json::Value = serde_json::from_slice(&reply.payload)?;
    anyhow::ensure!(
        value["ok"] == true,
        "{}",
        value["error"]
            .as_str()
            .unwrap_or("The game service is unavailable.")
    );
    Ok(())
}

async fn lookup_game(
    nats: &async_nats::Client,
    user: &twz_shared::GameUser,
) -> Result<Option<ExistingGameSummary>> {
    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        nats.request(
            SUBJ_GAME_CHECK,
            serde_json::to_vec(&ResumeGameRequest { user: user.clone() })?.into(),
        ),
    )
    .await??;
    let response: GameLookupResponse = serde_json::from_slice(&reply.payload)?;
    anyhow::ensure!(
        response.ok,
        "{}",
        response
            .error
            .unwrap_or_else(|| "Unable to check your existing town.".into())
    );
    Ok(response.game)
}

pub(super) async fn session(state: AppState, mut client: WebSocket) {
    let mut claimed: Option<GameConnection> = None;
    let mut is_new = false;
    let setup = tokio::time::timeout(Duration::from_secs(25), async {
        let Some(Ok(AxumMsg::Text(text))) = client.next().await else {
            anyhow::bail!("Game entry was interrupted.");
        };
        let entry: GameEntry = serde_json::from_str(&text)
            .map_err(|_| anyhow::anyhow!("Expected a game entry request."))?;
        let (token, name, quit, check, abandon_id) = match entry {
            GameEntry::CheckGame { id_token } => (id_token, None, false, true, None),
            GameEntry::StartGame {
                id_token,
                role,
                town_name,
            } => {
                anyhow::ensure!(
                    role == "mayor",
                    "Commando is not available yet. Please choose mayor."
                );
                (
                    id_token,
                    Some(validate_town_name(&town_name).map_err(anyhow::Error::msg)?),
                    false,
                    false,
                    None,
                )
            }
            GameEntry::ResumeGame { id_token } => (id_token, None, false, false, None),
            GameEntry::QuitGame { id_token } => (id_token, None, true, false, None),
            GameEntry::AbandonGame {
                id_token,
                player_id,
            } => (id_token, None, true, false, Some(player_id)),
        };
        let auth = state.auth.as_ref().ok_or_else(|| {
            anyhow::anyhow!("The gateway needs AUTH0_DOMAIN and AUTH0_CLIENT_ID configured.")
        })?;
        let user = auth.verify(&state.http, &token).await.map_err(|_| {
            anyhow::anyhow!("Your sign-in could not be verified. Sign out and sign in again.")
        })?;
        if check {
            let game = lookup_game(&state.nats, &user).await?;
            let message = match game {
                Some(game) => serde_json::json!({"type":"existing_game","game":game}),
                None => serde_json::json!({"type":"no_game"}),
            };
            client.send(AxumMsg::Text(message.to_string())).await?;
            return Ok(None);
        }
        if quit {
            operation(
                &state.nats,
                SUBJ_GAME_QUIT_USER,
                &QuitUserGameRequest {
                    user,
                    player_id: abandon_id,
                },
            )
            .await?;
            client
                .send(AxumMsg::Text("{\"type\":\"game_quit_ack\"}".into()))
                .await?;
            return Ok::<_, anyhow::Error>(None);
        }
        let lookup_user = user.clone();
        let reply = if let Some(desired_name) = name {
            is_new = true;
            state
                .nats
                .request(
                    SUBJ_TOWN_CREATE,
                    serde_json::to_vec(&CreateTownRequest {
                        player_id: uuid::Uuid::new_v4(),
                        desired_name,
                        user,
                    })?
                    .into(),
                )
                .await?
        } else {
            state
                .nats
                .request(
                    SUBJ_GAME_RESUME,
                    serde_json::to_vec(&ResumeGameRequest { user })?.into(),
                )
                .await?
        };
        let response: CreateTownResponse = serde_json::from_slice(&reply.payload)?;
        if !response.ok {
            if is_new {
                if let Some(game) = lookup_game(&state.nats, &lookup_user).await? {
                    client
                        .send(AxumMsg::Text(
                            serde_json::json!({"type":"existing_game","game":game}).to_string(),
                        ))
                        .await?;
                    return Ok(None);
                }
            }
            anyhow::bail!(
                "{}",
                response
                    .error
                    .unwrap_or_else(|| "Game entry failed.".into())
            );
        }
        let Some(town) = response.town else {
            client
                .send(AxumMsg::Text("{\"type\":\"no_game\"}".into()))
                .await?;
            return Ok(None);
        };
        let connection_id = response.connection_id.ok_or_else(|| {
            anyhow::anyhow!("The conductor must be updated for reconnect support.")
        })?;
        let checkpoint = response
            .player_state
            .unwrap_or_else(|| PlayerState::initial_for_town(&town));
        claimed = Some(GameConnection {
            player_id: town.mayor_player_id,
            connection_id,
            checkpoint: Some(checkpoint.clone()),
        });
        let cell = twz_shared::cell_of(checkpoint.x, checkpoint.y, CELL_TILES as f32 * TILE_PX);
        let owner = route_owner(&state, cell.0, cell.1).await?;
        let address = get_server_ws(&state, &owner).await?;
        let (backend, _) = tokio::time::timeout(
            IO_TIMEOUT,
            connect_async(format!("{}/ws", address.trim_end_matches('/'))),
        )
        .await??;
        Ok(Some((town, checkpoint, backend)))
    })
    .await;

    match setup {
        Ok(Ok(Some((town, checkpoint, backend)))) => {
            let mut connection = claimed.take().unwrap();
            let pid = connection.player_id.to_string();
            let latest = Arc::new(RwLock::new(Some(checkpoint.clone())));
            let context = RelayLease {
                nats: state.nats.clone(),
                connection: connection.clone(),
                latest: latest.clone(),
            };
            let (tx, rx) = mpsc::unbounded_channel();
            state.switches.write().await.insert(pid.clone(), tx);
            let relay = relay_with_context(
                client,
                backend,
                &pid,
                Some(&town),
                rx,
                |owner| {
                    let state = state.clone();
                    async move { get_server_ws(&state, &owner).await }
                },
                Some(&checkpoint),
                Some(context),
            );
            tokio::pin!(relay);
            let mut lease = tokio::time::interval_at(
                tokio::time::Instant::now() + Duration::from_secs(10),
                Duration::from_secs(10),
            );
            let outcome = loop {
                tokio::select! {
                    outcome=&mut relay=>break outcome,
                    _=lease.tick()=>{
                        connection.checkpoint=latest.read().await.clone();
                        if operation(&state.nats,SUBJ_GAME_TOUCH,&connection).await.is_err() {
                            break RelayOutcome {quit:false,checkpoint:connection.checkpoint.clone()};
                        }
                    }
                }
            };
            state.switches.write().await.remove(&pid);
            if !outcome.quit {
                connection.checkpoint = outcome.checkpoint;
                let _ = operation(&state.nats, SUBJ_GAME_DISCONNECT, &connection).await;
            }
        }
        Ok(Ok(None)) => {
            let _ = client.send(AxumMsg::Close(None)).await;
        }
        result => {
            let message = match result {
                Ok(Err(error)) => error.to_string(),
                _ => "Game entry timed out. Please try again.".into(),
            };
            let _ = client
                .send(AxumMsg::Text(
                    serde_json::json!({"type":"game_error","message":message}).to_string(),
                ))
                .await;
            let _ = client.send(AxumMsg::Close(None)).await;
            if let Some(connection) = claimed {
                if is_new {
                    let _ = operation(
                        &state.nats,
                        SUBJ_GAME_END,
                        &EndGameRequest {
                            player_id: connection.player_id,
                            connection_id: Some(connection.connection_id),
                            reason: GameEndReason::SetupFailed,
                        },
                    )
                    .await;
                } else {
                    let _ = operation(&state.nats, SUBJ_GAME_DISCONNECT, &connection).await;
                }
            }
        }
    }
}
