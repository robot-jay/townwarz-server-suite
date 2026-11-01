use super::*;

#[tokio::test]
#[ignore="requires TEST_DATABASE_URL and TEST_NATS_URL"]
async fn postgres_grace_resume_fencing_quit_orb_and_expiry()->Result<()> {
    let nats=async_nats::connect(std::env::var("TEST_NATS_URL")?).await?;
    let registry=TownRegistry::new(nats,&std::env::var("TEST_DATABASE_URL")?).await?;
    let user=GameUser {sub:format!("integration|{}",ClientId::new_v4()),name:Some("Mayor".into()),email:None};
    let request=||CreateTownRequest {player_id:ClientId::new_v4(),desired_name:"Copper Creek".into(),user:user.clone()};
    let town=registry.create(request()).await?;
    let initial=registry.response(town.clone()).await?;
    assert!(registry.resume(&user.sub).await.is_err(),"cannot attach two connections");
    let mut checkpoint=initial.player_state.unwrap();checkpoint.game.wallet.credit(500).unwrap();
    checkpoint.game.inventory.slots[0]=Some(ItemInstance {id:ClientId::new_v4(),item_type:ItemType::Wall,
        ownership:EntityOwnership {game_id:town.mayor_player_id,town_id:Some(town.id.clone()),owner_player_id:Some(town.mayor_player_id)},source_building_id:None});
    checkpoint.x+=100.0;
    let old=GameConnection {player_id:town.mayor_player_id,connection_id:initial.connection_id.unwrap(),checkpoint:Some(checkpoint.clone())};
    registry.connection_update(old.clone(),true).await?;
    assert_eq!(registry.list().await?.len(),1);
    assert!(registry.create(request()).await.is_err(),"disconnected hall is reserved");
    let resumed=registry.resume(&user.sub).await?;
    assert_eq!(resumed.player_state.as_ref(),Some(&checkpoint));
    assert_ne!(resumed.connection_id,Some(old.connection_id));
    assert!(registry.connection_update(old,true).await.is_err(),"stale socket cannot disconnect new session");
    registry.end_game(EndGameRequest {player_id:town.mayor_player_id,connection_id:resumed.connection_id,reason:GameEndReason::Orbed}).await?;
    assert!(registry.resume(&user.sub).await?.town.is_none());
    let second=registry.create(request()).await?;
    assert_eq!(second.hall.id,town.hall.id);
    let connection=registry.response(second.clone()).await?.connection_id;
    registry.end_game(EndGameRequest {player_id:second.mayor_player_id,connection_id:connection,reason:GameEndReason::IntentionalQuit}).await?;
    assert!(registry.resume(&user.sub).await?.town.is_none());
    let third=registry.create(request()).await?;
    registry.db.lock().await.execute("UPDATE twz_games SET last_seen_at=NOW()-INTERVAL '16 minutes' WHERE player_id=$1",&[&third.mayor_player_id.to_string()]).await?;
    assert!(registry.expire_leases().await?>=1);
    assert!(registry.resume(&user.sub).await?.town.is_none());
    let db=registry.db.lock().await;
    db.execute("DELETE FROM twz_games WHERE auth0_sub=$1",&[&user.sub]).await?;
    db.execute("DELETE FROM twz_users WHERE auth0_sub=$1",&[&user.sub]).await?;
    Ok(())
}
