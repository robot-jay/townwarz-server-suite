use anyhow::Result;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QuerySelect, Set,
};
use twz_shared::*;

use super::repository::INITIAL_HALL_POOL_SIZE;
use super::{
    entities::{games, users},
    GameRepository,
};

// Tests share an externally supplied development DB. Serialize their fixtures;
// each concurrency test still uses independent connections and simultaneous calls.
static FIXTURES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
#[ignore = "requires an isolated TEST_DATABASE_URL"]
async fn postgres_economy_commit_rollback_replay_and_stale_checkpoint_recovery() -> Result<()> {
    let _fixtures = FIXTURES.lock().await;
    let url = std::env::var("TEST_DATABASE_URL")?;
    let repository = GameRepository::connect(&url).await?;
    let account = user();
    let admission = repository
        .create_game(&account, ClientId::new_v4(), "Durable Economy")
        .await?;
    let id = admission.town.mayor_player_id;
    let token = admission.connection_id;
    let command = ConstructionCommand::PlaceBuilding {
        request_id: ClientId::new_v4(),
        building_type: BuildingType::WallResearch,
        origin_tile: IVec2 {
            x: admission.town.hall.origin.x - 3,
            y: admission.town.hall.origin.y,
        },
    };
    let mut tx = repository.begin_economy(id, token).await?.unwrap();
    tx.state.accrue(token, 1651000, IncomePolicy::default());
    assert_eq!(tx.state.cash, 55000);
    tx.commit().await?;
    // A lost preparation/failed database transaction has no economic effects.
    let mut uncommitted = repository.begin_economy(id, token).await?.unwrap();
    let (building, refund) = uncommitted.state.plan(&uncommitted.town, &command).unwrap();
    let result = uncommitted.state.apply_purchase(&command, building, refund);
    uncommitted.state.record(command.clone(), result);
    drop(uncommitted);
    let mut committed = repository.begin_economy(id, token).await?.unwrap();
    assert!(committed.state.replay(&command).is_none());
    assert_eq!(committed.state.cash, 55000);
    let (building, refund) = committed.state.plan(&committed.town, &command).unwrap();
    let result = committed.state.apply_purchase(&command, building, refund);
    let research_id = result.building_id.unwrap();
    committed.state.record(command.clone(), result);
    committed.commit().await?;
    // The commit happened, but its reply/application can be lost. Another
    // repository connection sees the durable result and reconstructed building.
    let reopened = GameRepository::connect(&url).await?;
    let tx = reopened.begin_economy(id, token).await?.unwrap();
    assert_eq!(tx.state.cash, 50000);
    assert_eq!(
        tx.state.replay(&command).unwrap().building_id,
        Some(research_id)
    );
    assert_eq!(tx.state.buildings.len(), 1);
    drop(tx);
    let mut old_checkpoint = admission.player_state;
    old_checkpoint.game.wallet.cash = 999999;
    repository
        .update_connection(
            &GameConnection {
                player_id: id,
                connection_id: token,
                checkpoint: Some(old_checkpoint),
            },
            true,
        )
        .await?;
    let resumed = reopened.resume_game(&account.sub).await?.unwrap();
    assert_eq!(
        resumed.player_state.game.wallet.cash, 50000,
        "old movement checkpoint cannot undo spending"
    );
    assert!(matches!(
        repository.begin_economy(id, token).await?,
        Err(ConstructionError::StaleConnection)
    ));
    let token = resumed.connection_id;
    let mut tx = reopened.begin_economy(id, token).await?.unwrap();
    tx.state.accrue(token, 1999, IncomePolicy::default());
    // The first connection left 1,000 ms partial progress; exactly another
    // 2,000 eligible ms earns the next 100, with no disconnected-time catchup.
    assert_eq!(tx.state.cash, 50000);
    tx.state.accrue(token, 2000, IncomePolicy::default());
    assert_eq!(tx.state.cash, 50100);
    tx.commit().await?;
    let demolition = ConstructionCommand::DemolishBuilding {
        request_id: ClientId::new_v4(),
        building_id: research_id,
    };
    let mut tx = reopened.begin_economy(id, token).await?.unwrap();
    let (_, refund) = tx.state.plan(&tx.town, &demolition).unwrap();
    assert_eq!(refund, 1250);
    let result = tx.state.apply_purchase(&demolition, None, refund);
    tx.state.record(demolition.clone(), result);
    tx.commit().await?;
    let tx = reopened.begin_economy(id, token).await?.unwrap();
    assert_eq!(tx.state.cash, 51350);
    assert_eq!(tx.state.replay(&demolition).unwrap().refund, 1250);
    assert!(tx.state.buildings.is_empty());
    drop(tx);
    repository
        .end_game(&EndGameRequest {
            player_id: id,
            connection_id: Some(token),
            reason: GameEndReason::IntentionalQuit,
        })
        .await?;
    assert!(!reopened
        .economy_projections()
        .await?
        .iter()
        .any(|p| p.town.mayor_player_id == id));
    assert!(matches!(
        repository.begin_economy(id, token).await?,
        Err(ConstructionError::InactiveGame)
    ));
    cleanup(&repository, &[&account.sub]).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an isolated TEST_DATABASE_URL"]
async fn postgres_economy_concurrent_spending_is_serialized_by_the_game_row() -> Result<()> {
    let _fixtures = FIXTURES.lock().await;
    let repository = GameRepository::connect(&std::env::var("TEST_DATABASE_URL")?).await?;
    let account = user();
    let admission = repository
        .create_game(&account, ClientId::new_v4(), "Concurrent Economy")
        .await?;
    let id = admission.town.mayor_player_id;
    let token = admission.connection_id;
    let mut tx = repository.begin_economy(id, token).await?.unwrap();
    tx.state.cash = 5000;
    tx.commit().await?;
    async fn purchase(repository: GameRepository, id: ClientId, token: ClientId) -> Result<bool> {
        let mut tx = repository.begin_economy(id, token).await?.unwrap();
        let command = ConstructionCommand::PlaceBuilding {
            request_id: ClientId::new_v4(),
            building_type: BuildingType::WallResearch,
            origin_tile: tx.town.hall.origin,
        };
        if let Ok((building, refund)) = tx.state.plan(&tx.town, &command) {
            let outcome = tx.state.apply_purchase(&command, building, refund);
            tx.state.record(command, outcome);
            tx.commit().await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    let (first, second) = tokio::join!(
        purchase(repository.clone(), id, token),
        purchase(repository.clone(), id, token)
    );
    assert_ne!(
        first?, second?,
        "only one of two individually affordable purchases can spend the same cash"
    );
    let tx = repository.begin_economy(id, token).await?.unwrap();
    assert_eq!(tx.state.cash, 0);
    assert_eq!(tx.state.buildings.len(), 1);
    drop(tx);
    cleanup(&repository, &[&account.sub]).await?;
    Ok(())
}

fn user() -> GameUser {
    GameUser {
        sub: format!("integration|{}", ClientId::new_v4()),
        name: Some("Mayor".into()),
        email: None,
    }
}

async fn cleanup(repository: &GameRepository, subjects: &[&str]) -> Result<()> {
    games::Entity::delete_many()
        .filter(games::Column::Auth0Sub.is_in(subjects.iter().copied()))
        .exec(&repository.db)
        .await?;
    users::Entity::delete_many()
        .filter(users::Column::Auth0Sub.is_in(subjects.iter().copied()))
        .exec(&repository.db)
        .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an isolated TEST_DATABASE_URL"]
async fn postgres_grace_resume_fencing_quit_orb_and_expiry() -> Result<()> {
    let _fixtures = FIXTURES.lock().await;
    let url = std::env::var("TEST_DATABASE_URL")?;
    let repository = GameRepository::connect(&url).await?;
    let mut user = user();
    let initial = repository
        .create_game(&user, ClientId::new_v4(), "Copper Creek")
        .await?;
    let town = initial.town;
    let summary = repository.active_game_summary(&user.sub).await?.unwrap();
    assert_eq!(summary.player_id, town.mayor_player_id);
    assert_eq!(summary.town_name, "Copper Creek");
    assert!(summary.is_connected);
    assert!(summary.reconnect_until_ms.is_none());
    assert!(repository
        .active_game_summary("unrelated|account")
        .await?
        .is_none());
    assert!(
        repository.resume_game(&user.sub).await.is_err(),
        "cannot attach two connections"
    );
    let mut checkpoint = initial.player_state;
    checkpoint.game.wallet.credit(500).unwrap();
    checkpoint.game.inventory.slots[0] = Some(ItemInstance {
        id: ClientId::new_v4(),
        item_type: ItemType::Wall,
        ownership: EntityOwnership {
            game_id: town.mayor_player_id,
            town_id: Some(town.id.clone()),
            owner_player_id: Some(town.mayor_player_id),
        },
        source_building_id: None,
    });
    checkpoint.x += 100.0;
    let old = GameConnection {
        player_id: town.mayor_player_id,
        connection_id: initial.connection_id,
        checkpoint: Some(checkpoint.clone()),
    };
    assert!(repository.update_connection(&old, true).await?);
    let summary = repository.active_game_summary(&user.sub).await?.unwrap();
    assert!(!summary.is_connected);
    assert!(summary.reconnect_until_ms.is_some());
    assert_eq!(
        games::Entity::find_by_id(town.mayor_player_id.to_string())
            .one(&repository.db)
            .await?
            .unwrap()
            .connection_id,
        None,
        "checking an existing game must not claim a connection"
    );
    assert!(repository
        .active_towns()
        .await?
        .iter()
        .any(|active| active.id == town.id));
    assert!(
        repository
            .create_game(&user, ClientId::new_v4(), "Cannot Replace")
            .await
            .is_err(),
        "disconnected hall is reserved"
    );

    // Startup migrations are idempotent over existing profiles/checkpoints.
    let reopened = GameRepository::connect(&url).await?;
    let resumed = reopened.resume_game(&user.sub).await?.unwrap();
    assert_eq!(resumed.player_state, checkpoint);
    assert_ne!(resumed.connection_id, old.connection_id);
    assert!(
        repository
            .update_connection(
                &GameConnection {
                    player_id: town.mayor_player_id,
                    connection_id: resumed.connection_id,
                    checkpoint: None,
                },
                false
            )
            .await?
    );
    let stored = games::Entity::find_by_id(town.mayor_player_id.to_string())
        .one(&repository.db)
        .await?
        .unwrap();
    assert_eq!(
        serde_json::from_value::<PlayerState>(stored.checkpoint_json.unwrap())?,
        checkpoint,
        "a heartbeat without a checkpoint must preserve the stored state"
    );
    assert!(
        !repository.update_connection(&old, true).await?,
        "stale disconnect cannot clear the resumed token"
    );
    assert!(
        !repository.update_connection(&old, false).await?,
        "stale heartbeat cannot overwrite the checkpoint"
    );
    let stale_end = EndGameRequest {
        player_id: town.mayor_player_id,
        connection_id: Some(old.connection_id),
        reason: GameEndReason::IntentionalQuit,
    };
    assert!(
        repository.end_game(&stale_end).await?.is_none(),
        "stale quit cannot end the new connection"
    );

    let end = EndGameRequest {
        player_id: town.mayor_player_id,
        connection_id: Some(resumed.connection_id),
        reason: GameEndReason::Orbed,
    };
    assert!(repository.end_game(&end).await?.is_some());
    assert!(
        repository.end_game(&end).await?.is_none(),
        "terminal transition is idempotent"
    );
    assert!(repository.resume_game(&user.sub).await?.is_none());
    assert!(
        !repository
            .update_connection(
                &GameConnection {
                    connection_id: resumed.connection_id,
                    ..old
                },
                false
            )
            .await?
    );

    user.name = Some("Renamed Mayor".into());
    user.email = Some("mayor@example.test".into());
    let second = repository
        .create_game(&user, ClientId::new_v4(), "New Town")
        .await?;
    assert_ne!(
        second.town.hall.id, town.hall.id,
        "fresh games avoid the account's previous hall"
    );
    assert_eq!(second.player_state.game.wallet.cash, 0);
    assert!(second
        .player_state
        .game
        .inventory
        .slots
        .iter()
        .all(Option::is_none));
    let profile = users::Entity::find_by_id(&user.sub)
        .one(&repository.db)
        .await?
        .unwrap();
    assert_eq!(profile.display_name, user.name);
    assert_eq!(profile.email, user.email);
    let end = EndGameRequest {
        player_id: second.town.mayor_player_id,
        connection_id: Some(second.connection_id),
        reason: GameEndReason::IntentionalQuit,
    };
    assert!(repository.end_game(&end).await?.is_some());
    assert!(repository.resume_game(&user.sub).await?.is_none());

    let third = repository
        .create_game(&user, ClientId::new_v4(), "Will Expire")
        .await?;
    games::Entity::update_many()
        .col_expr(
            games::Column::LastSeenAt,
            Expr::cust("CURRENT_TIMESTAMP - INTERVAL '16 minutes'"),
        )
        .filter(games::Column::PlayerId.eq(third.town.mayor_player_id.to_string()))
        .exec(&repository.db)
        .await?;
    assert!(repository
        .expire_games()
        .await?
        .iter()
        .any(|expired| expired.id == third.town.id));
    assert!(repository.resume_game(&user.sub).await?.is_none());
    assert!(repository.active_player_ids(&user.sub).await?.is_empty());
    let history = games::Entity::find()
        .select_only()
        .column(games::Column::PlayerId)
        .filter(games::Column::Auth0Sub.eq(&user.sub))
        .into_tuple::<String>()
        .all(&repository.db)
        .await?;
    assert_eq!(history.len(), 3, "history survives terminal transitions");
    cleanup(&repository, &[&user.sub]).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an isolated TEST_DATABASE_URL"]
async fn postgres_expiry_notices_are_once_scoped_and_bound_to_original_town() -> Result<()> {
    let _fixtures = FIXTURES.lock().await;
    let url = std::env::var("TEST_DATABASE_URL")?;
    let repository = GameRepository::connect(&url).await?;
    let account = user();
    let other = user();
    let initial = repository
        .create_game(&account, ClientId::new_v4(), "Forgotten Town")
        .await?;
    let old_town = initial.town.clone();
    let old_row = games::Entity::find_by_id(old_town.mayor_player_id.to_string())
        .one(&repository.db)
        .await?
        .unwrap();
    let mut checkpoint = initial.player_state;
    checkpoint.game.wallet.credit(1000).unwrap();
    checkpoint.game.inventory.slots[0] = Some(ItemInstance {
        id: ClientId::new_v4(),
        item_type: ItemType::Wall,
        ownership: EntityOwnership {
            game_id: old_town.mayor_player_id,
            town_id: Some(old_town.id.clone()),
            owner_player_id: Some(old_town.mayor_player_id),
        },
        source_building_id: None,
    });
    repository
        .update_connection(
            &GameConnection {
                player_id: old_town.mayor_player_id,
                connection_id: initial.connection_id,
                checkpoint: Some(checkpoint),
            },
            true,
        )
        .await?;
    games::Entity::update_many()
        .col_expr(
            games::Column::DisconnectedAt,
            Expr::cust("CURRENT_TIMESTAMP - INTERVAL '16 minutes'"),
        )
        .filter(games::Column::PlayerId.eq(old_town.mayor_player_id.to_string()))
        .exec(&repository.db)
        .await?;
    assert!(repository
        .expire_games()
        .await?
        .iter()
        .any(|town| town.id == old_town.id));
    let notice = repository.expired_game_notice(&account.sub).await?.unwrap();
    assert_eq!(notice.player_id, old_town.mayor_player_id);
    assert_eq!(notice.town_name, "Forgotten Town");
    assert_eq!(notice.grace_period_seconds, 900);
    assert!(!notice.former_town.is_active);
    assert!(!notice.former_town.rejoin_supported);
    assert_eq!(notice.former_town.roles[0].capacity, 1);
    assert_eq!(notice.former_town.roles[1].capacity, 4);
    assert!(notice.former_town.roles.iter().all(|role| !role.can_join));
    assert!(matches!(
        notice.former_town.state_on_join,
        RejoinStatePolicy::Fresh
    ));
    assert!(repository.resume_game(&account.sub).await?.is_none());
    repository
        .acknowledge_expiry(&other.sub, old_town.mayor_player_id)
        .await?;
    assert!(
        repository
            .expired_game_notice(&account.sub)
            .await?
            .is_some(),
        "another account cannot dismiss this notice"
    );

    // A new town occupying the old physical hall is not the original town.
    let replacement = repository
        .create_game(&other, ClientId::new_v4(), "Different Town")
        .await?;
    let mut replacement_town = replacement.town.clone();
    replacement_town.hall = old_town.hall.clone();
    let mut replacement_row =
        games::Entity::find_by_id(replacement.town.mayor_player_id.to_string())
            .one(&repository.db)
            .await?
            .unwrap()
            .into_active_model();
    replacement_row.hall_slot = Set(old_row.hall_slot);
    replacement_row.town_json = Set(serde_json::to_value(replacement_town)?);
    replacement_row.update(&repository.db).await?;
    assert!(
        !repository
            .expired_game_notice(&account.sub)
            .await?
            .unwrap()
            .former_town
            .is_active
    );
    assert!(repository.expired_game_notice(&other.sub).await?.is_none());

    repository
        .acknowledge_expiry(&account.sub, old_town.mayor_player_id)
        .await?;
    repository
        .acknowledge_expiry(&account.sub, old_town.mayor_player_id)
        .await?;
    let reopened = GameRepository::connect(&url).await?;
    assert!(
        reopened.expired_game_notice(&account.sub).await?.is_none(),
        "acknowledgement survives new connections/logins"
    );
    // A wall-clock correction must not make this older participation latest.
    // This deterministically reproduces the VM clock rollback seen in testing.
    games::Entity::update_many()
        .col_expr(
            games::Column::StartedAt,
            Expr::cust("CURRENT_TIMESTAMP + INTERVAL '1 day'"),
        )
        .filter(games::Column::PlayerId.eq(old_town.mayor_player_id.to_string()))
        .exec(&repository.db)
        .await?;
    let next = repository
        .create_game(&account, ClientId::new_v4(), "Fresh Participation")
        .await?;
    assert_ne!(next.town.mayor_player_id, old_town.mayor_player_id);
    assert_ne!(next.town.id, old_town.id);
    assert_eq!(next.player_state.game.wallet.cash, 0);
    assert!(next
        .player_state
        .game
        .inventory
        .slots
        .iter()
        .all(Option::is_none));
    assert_eq!(next.player_state.hp, next.player_state.max_hp);
    repository
        .end_game(&EndGameRequest {
            player_id: next.town.mayor_player_id,
            connection_id: Some(next.connection_id),
            reason: GameEndReason::Orbed,
        })
        .await?;
    assert!(
        repository
            .expired_game_notice(&account.sub)
            .await?
            .is_none(),
        "Orb defeat is not grace expiry"
    );

    // An unacknowledged older timeout must not override a later participation's
    // intentional departure when deciding what the returning player last lost.
    let older = repository
        .create_game(&account, ClientId::new_v4(), "Older Timeout")
        .await?;
    repository
        .end_game(&EndGameRequest {
            player_id: older.town.mayor_player_id,
            connection_id: Some(older.connection_id),
            reason: GameEndReason::Timeout,
        })
        .await?;
    assert!(
        repository
            .expired_game_notice(&account.sub)
            .await?
            .is_some(),
        "latest timeout missing from history: {:?}",
        games::Entity::find()
            .filter(games::Column::Auth0Sub.eq(&account.sub))
            .all(&repository.db)
            .await?
    );
    let latest = repository
        .create_game(&account, ClientId::new_v4(), "Later Departure")
        .await?;
    repository
        .end_game(&EndGameRequest {
            player_id: latest.town.mayor_player_id,
            connection_id: Some(latest.connection_id),
            reason: GameEndReason::IntentionalQuit,
        })
        .await?;
    assert!(repository
        .expired_game_notice(&account.sub)
        .await?
        .is_none());
    cleanup(&repository, &[&account.sub, &other.sub]).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an isolated TEST_DATABASE_URL"]
async fn postgres_concurrent_claims_and_resumes_use_database_locks() -> Result<()> {
    let _fixtures = FIXTURES.lock().await;
    let url = std::env::var("TEST_DATABASE_URL")?;
    let (left, right) = tokio::join!(GameRepository::connect(&url), GameRepository::connect(&url));
    let (left, right) = (left?, right?);
    let (a, b, c) = (user(), user(), user());
    let (first, second) = tokio::join!(
        left.create_game(&a, ClientId::new_v4(), "Concurrent A"),
        right.create_game(&b, ClientId::new_v4(), "Concurrent B"),
    );
    let (first, second) = (first?, second?);
    assert_ne!(first.town.hall.id, second.town.hall.id);

    let (attempt_a, attempt_b) = tokio::join!(
        left.create_game(&c, ClientId::new_v4(), "Same Account A"),
        right.create_game(&c, ClientId::new_v4(), "Same Account B"),
    );
    assert_ne!(
        attempt_a.is_ok(),
        attempt_b.is_ok(),
        "one account gets exactly one active game"
    );
    assert_eq!(left.active_player_ids(&c.sub).await?.len(), 1);

    let connection = GameConnection {
        player_id: first.town.mayor_player_id,
        connection_id: first.connection_id,
        checkpoint: Some(first.player_state.clone()),
    };
    assert!(left.update_connection(&connection, true).await?);
    let (attempt_a, attempt_b) = tokio::join!(left.resume_game(&a.sub), right.resume_game(&a.sub));
    assert_ne!(
        attempt_a.is_ok(),
        attempt_b.is_ok(),
        "only one connection can claim a disconnected game"
    );
    let resumed = attempt_a.or(attempt_b)?.unwrap();
    assert_eq!(resumed.player_state, first.player_state);
    assert!(!left.update_connection(&connection, true).await?);

    // A second heartbeat/disconnect cycle and concurrent reaping must leave the
    // fresh resumed game active, even across independent pools.
    assert!(
        left.update_connection(
            &GameConnection {
                connection_id: resumed.connection_id,
                ..connection.clone()
            },
            true
        )
        .await?
    );
    let (expired, resumed) = tokio::join!(left.expire_games(), right.resume_game(&a.sub));
    assert!(!expired?.iter().any(|town| town.id == first.town.id));
    assert!(resumed?.is_some());
    cleanup(&left, &[&a.sub, &b.sub, &c.sub]).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an isolated TEST_DATABASE_URL"]
async fn postgres_randomized_halls_avoid_previous_claims_and_expand_when_full() -> Result<()> {
    let _fixtures = FIXTURES.lock().await;
    let repository = GameRepository::connect(&std::env::var("TEST_DATABASE_URL")?).await?;
    let account = user();
    let mut previous = None;
    let mut seen = std::collections::HashSet::new();
    let mut base = None;
    for _ in 0..8 {
        let game = repository
            .create_game(&account, ClientId::new_v4(), "Random Start")
            .await?;
        let row = games::Entity::find_by_id(game.town.mayor_player_id.to_string())
            .one(&repository.db)
            .await?
            .unwrap();
        assert!((0..INITIAL_HALL_POOL_SIZE).contains(&row.hall_slot));
        assert_ne!(
            previous,
            Some(row.hall_slot),
            "consecutive new games must not reuse the previous hall"
        );
        assert_eq!(game.town.hall.id, hall_for_slot(row.hall_slot).id);
        previous = Some(row.hall_slot);
        seen.insert(row.hall_slot);
        repository
            .end_game(&EndGameRequest {
                player_id: game.town.mayor_player_id,
                connection_id: Some(game.connection_id),
                reason: GameEndReason::IntentionalQuit,
            })
            .await?;
        base = games::Entity::find_by_id(game.town.mayor_player_id.to_string())
            .one(&repository.db)
            .await?;
    }
    // This is deterministic because immediate repeats are excluded; no flaky
    // statistical claim about a small sample's frequency distribution is needed.
    assert!(seen.len() >= 2);
    let base = base.unwrap();

    // Any randomly chosen initial site must respect that site's previous claim
    // generation, even when its history is ahead of the current wall clock.
    let future = base.town_json["created_at_ms"].as_i64().unwrap() + 60_000;
    for slot in 0..INITIAL_HALL_POOL_SIZE {
        let id = ClientId::new_v4();
        let town = Town {
            id: format!("t-{id}"),
            name: "Historical Claim".into(),
            hall: hall_for_slot(slot),
            created_at_ms: future,
            mayor_player_id: id,
        };
        let mut history = base.clone().into_active_model();
        history.participation_order = sea_orm::ActiveValue::NotSet;
        history.player_id = Set(id.to_string());
        history.town_id = Set(town.id.clone());
        history.town_name = Set(town.name.clone());
        history.hall_slot = Set(slot);
        history.town_json = Set(serde_json::to_value(&town)?);
        history.checkpoint_json = Set(None);
        history.insert(&repository.db).await?;
    }
    let game = repository
        .create_game(&account, ClientId::new_v4(), "New Generation")
        .await?;
    assert!(game.town.created_at_ms > future);
    repository
        .end_game(&EndGameRequest {
            player_id: game.town.mayor_player_id,
            connection_id: Some(game.connection_id),
            reason: GameEndReason::IntentionalQuit,
        })
        .await?;

    // Fill the initial grid with distinct legitimate owners, including one
    // disconnected reservation, and verify allocation expands rather than
    // taking an occupied hall or reporting exhaustion.
    let mut owners = Vec::new();
    for slot in 0..INITIAL_HALL_POOL_SIZE {
        let owner = user();
        users::Entity::insert(users::ActiveModel {
            auth0_sub: Set(owner.sub.clone()),
            ..Default::default()
        })
        .exec(&repository.db)
        .await?;
        let id = ClientId::new_v4();
        let town = Town {
            id: format!("t-{id}"),
            name: "Reserved Hall".into(),
            hall: hall_for_slot(slot),
            created_at_ms: future + 1,
            mayor_player_id: id,
        };
        let mut occupied = base.clone().into_active_model();
        occupied.participation_order = sea_orm::ActiveValue::NotSet;
        occupied.player_id = Set(id.to_string());
        occupied.auth0_sub = Set(owner.sub.clone());
        occupied.town_id = Set(town.id.clone());
        occupied.town_name = Set(town.name.clone());
        occupied.hall_slot = Set(slot);
        occupied.town_json = Set(serde_json::to_value(&town)?);
        occupied.checkpoint_json = Set(Some(serde_json::to_value(PlayerState::initial_for_town(
            &town,
        ))?));
        occupied.ended_at = Set(None);
        occupied.end_reason = Set(None);
        occupied.connection_id = Set(Some(ClientId::new_v4().to_string()));
        occupied.insert(&repository.db).await?;
        owners.push(owner);
    }
    let reserved = games::Entity::find()
        .filter(games::Column::Auth0Sub.eq(&owners[0].sub))
        .one(&repository.db)
        .await?
        .unwrap();
    let reserved_id: ClientId = reserved.player_id.parse()?;
    assert!(
        repository
            .update_connection(
                &GameConnection {
                    player_id: reserved_id,
                    connection_id: reserved.connection_id.unwrap().parse()?,
                    checkpoint: None
                },
                true
            )
            .await?
    );
    let newcomer = repository
        .create_game(&account, ClientId::new_v4(), "Expanded Region")
        .await?;
    let allocated = games::Entity::find_by_id(newcomer.town.mayor_player_id.to_string())
        .one(&repository.db)
        .await?
        .unwrap();
    assert!((INITIAL_HALL_POOL_SIZE..INITIAL_HALL_POOL_SIZE + 2).contains(&allocated.hall_slot));
    let resumed = repository.resume_game(&owners[0].sub).await?.unwrap();
    assert_eq!(
        resumed.town.hall.id,
        hall_for_slot(0).id,
        "resume retains the original reserved hall"
    );
    let mut subjects: Vec<&str> = owners.iter().map(|owner| owner.sub.as_str()).collect();
    subjects.push(&account.sub);
    cleanup(&repository, &subjects).await?;
    Ok(())
}
