use super::*;
use twz_shared::{EntityOwnership, ItemInstance, ItemType, PlayerRole};

fn item(id: ClientId, identity: &GameIdentity) -> ItemInstance {
    ItemInstance {
        id: Uuid::new_v4(), item_type: ItemType::HomingMissile,
        ownership: EntityOwnership {
            game_id: identity.game_id, town_id: identity.town_id.clone(), owner_player_id: Some(id),
        }, source_building_id: Some(Uuid::new_v4()),
    }
}

fn credit(id: ClientId, identity: &GameIdentity) -> DamageAttribution {
    DamageAttribution {
        source_entity_id: id, source_kind: DamageSourceKind::Player,
        beneficiary: Some(identity.combatant(id)),
    }
}

#[test]
fn nonempty_player_state_survives_wire_handoff_and_backend_attachment() {
    let id = Uuid::new_v4();
    let identity = GameIdentity::mayor(id, "town-a".into());
    let mut player = Player::new(id, identity.clone(), Some(IVec2 { x: 17, y: 19 }));
    player.x = CELL_W + 100.0;
    player.y = 100.0;
    player.hp = 80;
    player.game.wallet.credit(125000).unwrap();
    player.game.inventory.slots[1] = Some(item(id, &identity));
    player.game.inventory.slots[5] = Some(item(id, &identity));
    player.game.cooldowns.primary_fire_ready_at_ms = 1234567;
    player.game.cooldowns.item_use_ready_at_ms = 1239999;
    player.damage_contributions.push(DamageContribution {
        credit: credit(Uuid::new_v4(), &GameIdentity::debug(Uuid::new_v4())),
        damage: 20, last_hit_at_ms: 1234,
    });
    let before = player.handoff_state(id);
    let message = HandoffRequest { from_server: "A".into(), to_server: "B".into(), player: before.clone(), ts_ms: 1 };
    let received: HandoffRequest = serde_json::from_slice(&serde_json::to_vec(&message).unwrap()).unwrap();
    let mut target = World::default();
    assert!(target.adopt_handoff(received.player));
    target.attach_player(id, None, identity.clone());
    assert_eq!(target.players[&id].handoff_state(id), before);

    // A duplicated old request must not revert an inventory or wallet mutation.
    target.players.get_mut(&id).unwrap().game.wallet.credit(1).unwrap();
    assert!(target.adopt_handoff(before.clone()));
    assert_eq!(target.players[&id].game.wallet.cash, 125001);
    let mut wrong_game = before;
    wrong_game.game.identity.game_id = Uuid::new_v4();
    assert!(!target.adopt_handoff(wrong_game));
    assert_eq!(target.players[&id].game.identity, identity);
}

#[test]
fn combat_death_clears_inventory_once_preserves_cash_and_records_assists() {
    let id = Uuid::new_v4();
    let identity = GameIdentity::mayor(id, "defenders".into());
    let mut player = Player::new(id, identity.clone(), None);
    player.game.wallet.credit(500).unwrap();
    player.game.inventory.slots[0] = Some(item(id, &identity));
    let first = credit(Uuid::new_v4(), &GameIdentity::mayor(Uuid::new_v4(), "attackers".into()));
    let mut lethal = credit(Uuid::new_v4(), &GameIdentity::mayor(Uuid::new_v4(), "attackers".into()));
    lethal.source_kind = DamageSourceKind::Turret;
    lethal.source_entity_id = Uuid::new_v4();
    player.take_damage(id, 20, &first, 1000);
    assert!(player.game.inventory.slots[0].is_some());
    let events = player.take_damage(id, 1000, &lethal, 1500);
    assert_eq!(player.hp, 0);
    assert_eq!(player.respawn_at, Some(4500));
    assert!(player.game.inventory.slots.iter().all(Option::is_none));
    assert_eq!(player.game.wallet.cash, 500);
    let record = events.iter().find_map(|event| match event { ServerEvent::Defeat { record } => Some(record), _ => None }).unwrap();
    assert_eq!(record.credit, lethal);
    assert_eq!(record.target_game_id, identity.game_id);
    assert_eq!(record.contributions.len(), 2);
    assert_eq!(record.contributions.iter().map(|c| c.damage).sum::<u32>(), 100);
    assert!(player.take_damage(id, 20, &lethal, 1600).is_empty(), "no second defeat or payout event");
    let new_game = Player::new(Uuid::new_v4(), GameIdentity::debug(Uuid::new_v4()), None);
    assert_eq!(new_game.game.wallet.cash, 0);
    assert!(new_game.game.inventory.slots.iter().all(Option::is_none));
}

#[test]
fn projectile_allegiance_and_credit_survive_shooter_removal() {
    let owner_id = Uuid::new_v4();
    let owner = GameIdentity::mayor(owner_id, "town-a".into());
    let projectile = Bullet {
        id: 1, x: 0.0, y: 0.0, vx: 300.0, vy: 0.0, owner: owner_id,
        born_ms: 0, credit: credit(owner_id, &owner), previous_x:0.0, previous_y:0.0,
    };
    let mut world = World::default();
    world.attach_player(owner_id, None, owner.clone());
    world.remove_player(owner_id);
    let ally_id = Uuid::new_v4();
    let mut ally = Player::new(ally_id, GameIdentity { game_id: Uuid::new_v4(), town_id: Some("town-a".into()), role: Some(PlayerRole::Commando) }, None);
    assert!(ally.take_damage(ally_id, 20, &projectile.credit, 1000).is_empty());
    assert_eq!(ally.hp, 100);
    let enemy_id = Uuid::new_v4();
    let mut enemy = Player::new(enemy_id, GameIdentity::mayor(enemy_id, "town-b".into()), None);
    assert!(!enemy.take_damage(enemy_id, 20, &projectile.credit, 1000).is_empty());
    assert_eq!(enemy.hp, 80);
    assert_eq!(enemy.damage_contributions[0].credit.beneficiary.as_ref().unwrap().game_id, owner_id);
}

#[test]
fn debug_players_are_mutual_enemies_and_town_allies_pass_through() {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let mut world = World::default();
    world.attach_player(b, None, GameIdentity::debug(b));
    world.players.get_mut(&b).unwrap().x = 10.0;
    world.players.get_mut(&b).unwrap().y = 0.0;
    let (mut x, mut y) = (0.0, 0.0);
    resolve_vs_enemy_players(a, &GameIdentity::debug(a).combatant(a), &mut x, &mut y, &world.players);
    assert!(x < 0.0);
    let bullet = credit(a, &GameIdentity::debug(a));
    assert!(!world.players.get_mut(&b).unwrap().take_damage(b, 20, &bullet, 1).is_empty());
    world.players.get_mut(&b).unwrap().game.identity = GameIdentity::mayor(b, "allies".into());
    (x, y) = (0.0, 0.0);
    resolve_vs_enemy_players(a, &GameIdentity::mayor(a, "allies".into()).combatant(a), &mut x, &mut y, &world.players);
    assert_eq!((x, y), (0.0, 0.0));
}

#[test]
fn rejected_or_unrelated_handoff_ack_cannot_delete_the_wallet() {
    let id = Uuid::new_v4();
    let mut world = World::default();
    world.attach_player(id, None, GameIdentity::debug(id));
    let player = world.players.get_mut(&id).unwrap();
    player.game.wallet.credit(100).unwrap();
    player.transferring = true;
    player.transfer_target = Some("B".into());
    let mut ack = HandoffAck { to_server: "C".into(), from_server: "A".into(), player_id: id, accepted: true, ts_ms: 0 };
    assert!(!world.complete_handoff(&ack));
    ack.to_server = "B".into();
    ack.accepted = false;
    assert!(!world.complete_handoff(&ack));
    assert_eq!(world.players[&id].game.wallet.cash, 100);
    assert!(!world.players[&id].transferring);
}

#[test]
fn ending_a_game_removes_assets_and_stale_directory_cannot_reclaim_its_hall() {
    use twz_shared::{BuildingType,BuildingInstance,Health,ItemBuildingInstance,ItemBuildingType,DeployedState,WorldPoint,hall_entity_id};
    let id=Uuid::new_v4();
    let identity=GameIdentity::mayor(id,"town-a".into());
    let town=Town {id:"town-a".into(),name:"Town A".into(),hall:twz_shared::hall_for_slot(0),created_at_ms:1,mayor_player_id:id};
    let mut world=World::default();world.attach_player(id,Some(town.clone()),identity.clone());
    let owner=EntityOwnership {game_id:id,town_id:Some(town.id.clone()),owner_player_id:Some(id)};
    let factory_id=Uuid::new_v4();
    world.insert_building(BuildingInstance {id:factory_id,building_type:BuildingType::WallFactory,ownership:Some(owner.clone()),
        origin_tile:IVec2{x:13,y:16},health:Health{current:255,maximum:255},production:None}).unwrap();
    let pickup=item(id,&identity);world.buildings.items.insert(pickup.id,pickup);
    let deployment_id=Uuid::new_v4();
    world.buildings.deployments.insert(deployment_id,ItemBuildingInstance {id:deployment_id,item_building_type:ItemBuildingType::Wall,
        ownership:owner,position:WorldPoint{x:1000.0,y:1000.0},angle_radians:0.0,health:Health{current:100,maximum:100},behavior:DeployedState::Wall});
    let old_request=Instant::now();
    let saved=world.players[&id].handoff_state(id);
    world.end_game(id);
    world.sync_towns(vec![town.clone()],old_request);
    assert!(!world.buildings.buildings.contains_key(&factory_id));
    assert!(world.buildings.items.is_empty() && world.buildings.deployments.is_empty());
    assert!(!world.adopt_handoff(saved));
    let hall=&world.buildings.buildings[&hall_entity_id(town.hall.origin)];
    assert!(hall.ownership.is_none());
    assert!(world.towns.is_empty());
    let mut replacement=town;replacement.id="town-b".into();replacement.created_at_ms+=1;replacement.mayor_player_id=Uuid::new_v4();
    world.attach_player(replacement.mayor_player_id,Some(replacement.clone()),GameIdentity::mayor(replacement.mayor_player_id,replacement.id.clone()));
    world.end_game(id);
    assert_eq!(world.buildings.buildings[&hall_entity_id(replacement.hall.origin)].ownership.as_ref().unwrap().game_id,replacement.mayor_player_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an isolated TEST_NATS_URL"]
async fn live_handoff_preserves_nonempty_state_through_nats_and_websocket_attach() -> anyhow::Result<()> {
    use axum::{extract::Path, routing::post, Json};
    use tokio::{net::TcpListener, time::timeout};
    use tokio_tungstenite::{connect_async, tungstenite::Message as ClientFrame};

    let nats = async_nats::connect(std::env::var("TEST_NATS_URL")?).await?;
    let conductor = TcpListener::bind("127.0.0.1:0").await?;
    let conductor_url = format!("http://{}", conductor.local_addr()?);
    let app = Router::new()
        .route("/heartbeat", post(|| async { Json(serde_json::json!({"ok":true})) }))
        .route("/route/cell/:cx/:cy", get(|Path((cx, _cy)): Path<(i32, i32)>| async move {
            Json(RouteCellResponse { owner_server_id: if cx >= 1 { "test-B" } else { "test-A" }.into() })
        }));
    let conductor_task = tokio::spawn(async move { axum::serve(conductor, app).await.unwrap() });

    let mut cells = Vec::new();
    let mut tasks = Vec::new();
    for server_id in ["test-A", "test-B"] {
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
        let state = AppState {
            server_id: server_id.into(), conductor: conductor_url.clone(), nats: nats.clone(),
            clients: Arc::new(RwLock::new(HashMap::new())), world: Arc::new(RwLock::new(World::default())),
            input_tx, ctrl_tx,
        };
        let req = nats.subscribe(format!("handoff.{server_id}.req")).await?;
        let ack = nats.subscribe(format!("handoff.{server_id}.ack")).await?;
        tasks.push(tokio::spawn(handoff_req_consumer(state.clone(), req)));
        tasks.push(tokio::spawn(handoff_ack_consumer(state.clone(), ack)));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}/ws", listener.local_addr()?);
        let app = Router::new().route("/ws", get(ws_handler)).with_state(state.clone());
        tasks.push(tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }));
        cells.push((state, url, input_rx, ctrl_rx));
    }
    let id = Uuid::new_v4();
    let identity = GameIdentity::mayor(id, "transfer-town".into());
    let mut player = Player::new(id, identity.clone(), Some(IVec2 { x:17, y:19 }));
    player.x = CELL_W + 17.0 * TILE_PX;
    player.y = 19.0 * TILE_PX;
    player.last_cell = (0,0);
    player.game.wallet.credit(75000).map_err(anyhow::Error::msg)?;
    player.game.inventory.slots[2] = Some(item(id, &identity));
    player.game.cooldowns.primary_fire_ready_at_ms = 120000;
    player.game.cooldowns.item_use_ready_at_ms = 130000;
    let expected = player.game.clone();
    cells[0].0.world.write().await.players.insert(id, player);
    let mut switches = nats.subscribe(format!("switch.{id}")).await?;
    nats.flush().await?;
    let endpoints: Vec<String> = cells.iter().map(|(_, url, _, _)| url.clone()).collect();
    let source_world = cells[0].0.world.clone();
    let source_clients = cells[0].0.clients.clone();
    let (mut source, _) = connect_async(&endpoints[0]).await?;
    let hello = serde_json::to_string(&BackendHello::SetPid { id, town:None, identity:Some(identity), resume_state:None })?;
    source.send(ClientFrame::Text(hello.clone())).await?;
    timeout(Duration::from_secs(5), async {
        while !source_clients.read().await.contains_key(&id) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await?;
    for (state, url, inputs, controls) in cells {
        let _ = url;
        tasks.push(tokio::spawn(game_loop(state, inputs, controls, reqwest::Client::new())));
    }
    timeout(Duration::from_secs(5), switches.next()).await?.ok_or_else(|| anyhow::anyhow!("no switch"))?;
    assert!(!source_world.read().await.players.contains_key(&id));
    let (mut destination, _) = connect_async(&endpoints[1]).await?;
    destination.send(ClientFrame::Text(hello)).await?;
    let received = timeout(Duration::from_secs(5), async {
        loop {
            let frame = destination.next().await.unwrap().unwrap();
            if let ClientFrame::Text(text) = frame {
                let state: serde_json::Value = serde_json::from_str(&text).unwrap();
                if state["type"] == "state" { return state; }
            }
        }
    }).await?;
    let actual: PlayerGameState = serde_json::from_value(received["self_state"].clone())?;
    assert_eq!(actual, expected);
    assert_eq!(received["catalog_version"], DOMAIN_VERSION);
    assert_eq!(received["player_affiliations"][0]["town_id"], "transfer-town");
    source.close(None).await?;
    destination.close(None).await?;
    for task in tasks { task.abort(); }
    conductor_task.abort();
    Ok(())
}
