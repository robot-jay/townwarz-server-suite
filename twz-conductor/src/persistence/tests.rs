use anyhow::Result;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use sea_orm::sea_query::Expr;
use twz_shared::*;

use super::{entities::{games, users}, GameRepository};

// Tests share an externally supplied development DB. Serialize their fixtures;
// each concurrency test still uses independent connections and simultaneous calls.
static FIXTURES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn user() -> GameUser {
    GameUser { sub: format!("integration|{}", ClientId::new_v4()), name: Some("Mayor".into()), email: None }
}

async fn cleanup(repository: &GameRepository, subjects: &[&str]) -> Result<()> {
    games::Entity::delete_many().filter(games::Column::Auth0Sub.is_in(subjects.iter().copied()))
        .exec(&repository.db).await?;
    users::Entity::delete_many().filter(users::Column::Auth0Sub.is_in(subjects.iter().copied()))
        .exec(&repository.db).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an isolated TEST_DATABASE_URL"]
async fn postgres_grace_resume_fencing_quit_orb_and_expiry() -> Result<()> {
    let _fixtures = FIXTURES.lock().await;
    let url = std::env::var("TEST_DATABASE_URL")?;
    let repository = GameRepository::connect(&url).await?;
    let mut user = user();
    let initial = repository.create_game(&user, ClientId::new_v4(), "Copper Creek").await?;
    let town = initial.town;
    assert!(repository.resume_game(&user.sub).await.is_err(), "cannot attach two connections");
    let mut checkpoint = initial.player_state;
    checkpoint.game.wallet.credit(500).unwrap();
    checkpoint.game.inventory.slots[0] = Some(ItemInstance {
        id: ClientId::new_v4(), item_type: ItemType::Wall,
        ownership: EntityOwnership { game_id: town.mayor_player_id, town_id: Some(town.id.clone()), owner_player_id: Some(town.mayor_player_id) },
        source_building_id: None,
    });
    checkpoint.x += 100.0;
    let old = GameConnection { player_id: town.mayor_player_id, connection_id: initial.connection_id, checkpoint: Some(checkpoint.clone()) };
    assert!(repository.update_connection(&old, true).await?);
    assert!(repository.active_towns().await?.iter().any(|active| active.id == town.id));
    assert!(repository.create_game(&user, ClientId::new_v4(), "Cannot Replace").await.is_err(), "disconnected hall is reserved");

    // Startup migrations are idempotent over existing profiles/checkpoints.
    let reopened = GameRepository::connect(&url).await?;
    let resumed = reopened.resume_game(&user.sub).await?.unwrap();
    assert_eq!(resumed.player_state, checkpoint);
    assert_ne!(resumed.connection_id, old.connection_id);
    assert!(repository.update_connection(&GameConnection {
        player_id: town.mayor_player_id, connection_id: resumed.connection_id, checkpoint: None,
    }, false).await?);
    let stored = games::Entity::find_by_id(town.mayor_player_id.to_string())
        .one(&repository.db).await?.unwrap();
    assert_eq!(serde_json::from_value::<PlayerState>(stored.checkpoint_json.unwrap())?, checkpoint,
        "a heartbeat without a checkpoint must preserve the stored state");
    assert!(!repository.update_connection(&old, true).await?, "stale disconnect cannot clear the resumed token");
    assert!(!repository.update_connection(&old, false).await?, "stale heartbeat cannot overwrite the checkpoint");
    let stale_end = EndGameRequest { player_id: town.mayor_player_id, connection_id: Some(old.connection_id), reason: GameEndReason::IntentionalQuit };
    assert!(repository.end_game(&stale_end).await?.is_none(), "stale quit cannot end the new connection");

    let end = EndGameRequest { player_id: town.mayor_player_id, connection_id: Some(resumed.connection_id), reason: GameEndReason::Orbed };
    assert!(repository.end_game(&end).await?.is_some());
    assert!(repository.end_game(&end).await?.is_none(), "terminal transition is idempotent");
    assert!(repository.resume_game(&user.sub).await?.is_none());
    assert!(!repository.update_connection(&GameConnection { connection_id: resumed.connection_id, ..old }, false).await?);

    user.name = Some("Renamed Mayor".into());
    user.email = Some("mayor@example.test".into());
    let second = repository.create_game(&user, ClientId::new_v4(), "New Town").await?;
    assert_eq!(second.town.hall.id, town.hall.id);
    assert!(second.town.created_at_ms > town.created_at_ms);
    assert_eq!(second.player_state.game.wallet.cash, 0);
    assert!(second.player_state.game.inventory.slots.iter().all(Option::is_none));
    let profile = users::Entity::find_by_id(&user.sub).one(&repository.db).await?.unwrap();
    assert_eq!(profile.display_name, user.name);
    assert_eq!(profile.email, user.email);
    let end = EndGameRequest { player_id: second.town.mayor_player_id, connection_id: Some(second.connection_id), reason: GameEndReason::IntentionalQuit };
    assert!(repository.end_game(&end).await?.is_some());
    assert!(repository.resume_game(&user.sub).await?.is_none());

    let third = repository.create_game(&user, ClientId::new_v4(), "Will Expire").await?;
    games::Entity::update_many()
        .col_expr(games::Column::LastSeenAt, Expr::cust("CURRENT_TIMESTAMP - INTERVAL '16 minutes'"))
        .filter(games::Column::PlayerId.eq(third.town.mayor_player_id.to_string()))
        .exec(&repository.db).await?;
    assert!(repository.expire_games().await?.iter().any(|expired| expired.id == third.town.id));
    assert!(repository.resume_game(&user.sub).await?.is_none());
    assert!(repository.active_player_ids(&user.sub).await?.is_empty());
    let history = games::Entity::find().select_only().column(games::Column::PlayerId)
        .filter(games::Column::Auth0Sub.eq(&user.sub)).into_tuple::<String>().all(&repository.db).await?;
    assert_eq!(history.len(), 3, "history survives terminal transitions");
    cleanup(&repository, &[&user.sub]).await?;
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
    assert_ne!(attempt_a.is_ok(), attempt_b.is_ok(), "one account gets exactly one active game");
    assert_eq!(left.active_player_ids(&c.sub).await?.len(), 1);

    let connection = GameConnection { player_id: first.town.mayor_player_id, connection_id: first.connection_id, checkpoint: Some(first.player_state.clone()) };
    assert!(left.update_connection(&connection, true).await?);
    let (attempt_a, attempt_b) = tokio::join!(left.resume_game(&a.sub), right.resume_game(&a.sub));
    assert_ne!(attempt_a.is_ok(), attempt_b.is_ok(), "only one connection can claim a disconnected game");
    let resumed = attempt_a.or(attempt_b)?.unwrap();
    assert_eq!(resumed.player_state, first.player_state);
    assert!(!left.update_connection(&connection, true).await?);

    // A second heartbeat/disconnect cycle and concurrent reaping must leave the
    // fresh resumed game active, even across independent pools.
    assert!(left.update_connection(&GameConnection { connection_id: resumed.connection_id, ..connection.clone() }, true).await?);
    let (expired, resumed) = tokio::join!(left.expire_games(), right.resume_game(&a.sub));
    assert!(!expired?.iter().any(|town| town.id == first.town.id));
    assert!(resumed?.is_some());
    cleanup(&left, &[&a.sub, &b.sub, &c.sub]).await?;
    Ok(())
}
