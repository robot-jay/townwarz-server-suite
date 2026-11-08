//! Async command coordinator. Durable row locks serialize economic effects;
//! cells reserve spatial changes through their simulation control queue.
use crate::{persistence::GameRepository, AppState};
use anyhow::Result;
use futures_util::StreamExt;
use std::time::Duration;
use twz_shared::*;

pub async fn serve(
    nats: async_nats::Client,
    games: GameRepository,
    routing: AppState,
) -> Result<()> {
    // Restore the home-cell bindings before routing becomes available. New
    // registrations must not silently move durable infrastructure to a second
    // authority. Restart the configured IDs; live reassignment remains Phase 6.
    for projection in games.economy_projections().await? {
        if let Some(owner) = projection.authority {
            let mut registry = routing.inner.write().await;
            if let Some(existing) = registry
                .owners
                .insert(projection.town.hall.cell, owner.clone())
            {
                anyhow::ensure!(existing == owner, "Conflicting durable cell authorities");
            }
        }
    }
    routing.inner.write().await.routing_ready = true;
    let mut commands = nats.subscribe(SUBJ_ECONOMY).await?;
    let mut worlds = nats.subscribe(SUBJ_ECONOMY_WORLD).await?;
    let policy = IncomePolicy {
        amount: env_number("TWZ_MAYOR_INCOME_AMOUNT", 100)?,
        interval_ms: env_number("TWZ_MAYOR_INCOME_INTERVAL_MS", 3000)?,
    };
    anyhow::ensure!(
        policy.interval_ms > 0 && policy.amount <= Wallet::MAX_CASH,
        "Invalid Mayor income settings"
    );
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(message) = commands.next() => {
                    let response = match serde_json::from_slice::<EconomyRequest>(&message.payload) {
                        Ok(request) => match execute(&nats, &games, &routing, policy, request).await {
                            Ok(response) => response,
                            Err(error) => {
                                tracing::warn!("economy command failed: {error:#}");
                                EconomyResponse { error: Some(ConstructionError::ServiceUnavailable), result: None, projection: None }
                            }
                        },
                        Err(_) => EconomyResponse { error: Some(ConstructionError::ServiceUnavailable), result: None, projection: None },
                    };
                    if let Some(reply) = message.reply { let _ = nats.publish(reply, serde_json::to_vec(&response).unwrap().into()).await; }
                }
                Some(message) = worlds.next() => {
                    match games.economy_projections().await {
                        Ok(world) => if let Some(reply) = message.reply { let _ = nats.publish(reply, serde_json::to_vec(&world).unwrap().into()).await; },
                        Err(error) => tracing::warn!("economy reconciliation failed: {error:#}"),
                    }
                }
                else => break,
            }
        }
    });
    Ok(())
}
fn env_number(name: &str, default: u64) -> Result<u64> {
    match std::env::var(name) {
        Ok(value) => Ok(value.parse()?),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(e) => Err(e.into()),
    }
}
async fn authority(routing: &AppState, cell: CellKey) -> Option<String> {
    crate::owner_for_cell(routing, cell).await
}
async fn execute(
    nats: &async_nats::Client,
    games: &GameRepository,
    routing: &AppState,
    policy: IncomePolicy,
    request: EconomyRequest,
) -> Result<EconomyResponse> {
    let mut tx = match games
        .begin_economy(request.player_id, request.connection_id)
        .await?
    {
        Ok(tx) => tx,
        Err(error) => {
            return Ok(EconomyResponse {
                error: Some(error),
                result: request
                    .command
                    .as_ref()
                    .map(|command| ConstructionResult::rejected(command, error, 0)),
                projection: None,
            })
        }
    };
    tx.state
        .accrue(request.connection_id, request.elapsed_ms, policy);
    let owner = authority(routing, tx.town.hall.cell).await;
    if tx.state.authority.is_none() {
        tx.state.authority = owner.clone();
    }
    let mut result = None;
    if let Some(command) = request.command {
        if let Some(replay) = tx.state.replay(&command) {
            result = Some(replay);
        } else {
            let planned = tx.state.plan(&tx.town, &command);
            let error = if owner.is_none() || owner != tx.state.authority {
                Some(ConstructionError::AuthorityUnavailable)
            } else if let Err(error) = &planned {
                Some(*error)
            } else {
                let preparation = ConstructionPreparation {
                    command: command.clone(),
                    projection: tx.projection(),
                    candidate: planned.as_ref().unwrap().0.clone(),
                };
                match tokio::time::timeout(
                    Duration::from_secs(3),
                    nats.request(
                        subj_construction_prepare(owner.as_ref().unwrap()),
                        serde_json::to_vec(&preparation)?.into(),
                    ),
                )
                .await
                {
                    Ok(Ok(reply)) => {
                        serde_json::from_slice::<ConstructionPrepared>(&reply.payload)?.error
                    }
                    _ => Some(ConstructionError::ServiceUnavailable),
                }
            };
            let outcome = if let Some(error) = error {
                ConstructionResult::rejected(&command, error, tx.state.cash)
            } else {
                let (building, refund) = planned.unwrap();
                tx.state.apply_purchase(&command, building, refund)
            };
            tx.state.record(command, outcome.clone());
            result = Some(outcome);
        }
    }
    let projection = tx.commit().await?;
    // Delivery may fail after commit. Cells reconcile from PostgreSQL projections
    // independently; a retry returns the durable outcome, never repeats effects.
    if let Some(owner) = &projection.authority {
        let _ = nats
            .publish(
                subj_economy_apply(owner),
                serde_json::to_vec(&projection)?.into(),
            )
            .await;
    }
    Ok(EconomyResponse {
        error: None,
        result,
        projection: Some(projection),
    })
}
