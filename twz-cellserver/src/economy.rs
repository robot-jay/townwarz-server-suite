use super::*;
use twz_shared::*;

#[derive(Clone)]
pub(super) struct PendingConstruction {
    started: Instant,
    base_revision: u64,
    reservation: Option<Uuid>,
}

pub(super) async fn serve(state: AppState) -> anyhow::Result<()> {
    let mut preparations = state
        .nats
        .subscribe(subj_construction_prepare(&state.server_id))
        .await?;
    let mut applications = state
        .nats
        .subscribe(subj_economy_apply(&state.server_id))
        .await?;
    let prepare_state = state.clone();
    tokio::spawn(async move {
        while let Some(message) = preparations.next().await {
            let error = match serde_json::from_slice::<ConstructionPreparation>(&message.payload) {
                Ok(preparation) => {
                    let (reply, received) = tokio::sync::oneshot::channel();
                    if prepare_state
                        .ctrl_tx
                        .send(ControlMsg::PrepareConstruction { preparation, reply })
                        .is_err()
                    {
                        Some(ConstructionError::ServiceUnavailable)
                    } else {
                        received
                            .await
                            .unwrap_or(Some(ConstructionError::ServiceUnavailable))
                    }
                }
                Err(_) => Some(ConstructionError::ServiceUnavailable),
            };
            if let Some(reply) = message.reply {
                let _ = prepare_state
                    .nats
                    .publish(
                        reply,
                        serde_json::to_vec(&ConstructionPrepared { error })
                            .unwrap()
                            .into(),
                    )
                    .await;
            }
        }
    });
    let apply_state = state.clone();
    tokio::spawn(async move {
        while let Some(message) = applications.next().await {
            if let Ok(projection) = serde_json::from_slice(&message.payload) {
                let _ = apply_state.ctrl_tx.send(ControlMsg::ApplyEconomy {
                    projection,
                    requested_at: None,
                });
            }
        }
    });
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        loop {
            interval.tick().await;
            let started = Instant::now();
            if let Ok(Ok(message)) = tokio::time::timeout(
                Duration::from_secs(5),
                state.nats.request(SUBJ_ECONOMY_WORLD, "".into()),
            )
            .await
            {
                if let Ok(projections) =
                    serde_json::from_slice::<Vec<EconomyProjection>>(&message.payload)
                {
                    for projection in projections {
                        let _ = state.ctrl_tx.send(ControlMsg::ApplyEconomy {
                            projection,
                            requested_at: Some(started),
                        });
                    }
                }
            }
        }
    });
    Ok(())
}

impl World {
    pub(super) fn construction_status(&self, id: ClientId) -> ConstructionStatus {
        let player = self.players.get(&id);
        let town = self.towns.iter().find(|town| town.mayor_player_id == id);
        let territory = town.map(TownTerritory::for_town);
        let reason = match (player, territory) {
            (Some(player), Some(territory)) => {
                if player.game.identity.role != Some(PlayerRole::Mayor) {
                    Some(ConstructionError::NotMayor)
                } else if player.hp <= 0 {
                    Some(ConstructionError::NotAlive)
                } else if player.transferring || player.economy_pending {
                    Some(ConstructionError::Busy)
                } else if !territory.contains_point(player.x, player.y) {
                    Some(ConstructionError::OutsideTerritory)
                } else {
                    None
                }
            }
            _ => Some(ConstructionError::InactiveGame),
        };
        ConstructionStatus {
            territory,
            can_construct: reason.is_none(),
            reason,
        }
    }
    pub(super) fn visible_buildings(&self, bounds: WorldRect) -> Vec<BuildingView> {
        let reserved: HashSet<_> = self
            .pending_construction
            .values()
            .filter_map(|p| p.reservation)
            .collect();
        self.buildings
            .views(bounds)
            .into_iter()
            .filter(|view| !reserved.contains(&view.instance.id))
            .collect()
    }
    pub(super) fn apply_economy(
        &mut self,
        projection: EconomyProjection,
        server_id: &str,
        requested_at: Option<Instant>,
    ) {
        let game_id = projection.town.mayor_player_id;
        if self.ended_games.contains(&game_id)
            || self
                .economy_revisions
                .get(&game_id)
                .is_some_and(|rev| *rev > projection.revision)
        {
            return;
        }
        // An unsolicited late notification is not sufficient to recreate a
        // town on a freshly restarted cell. Bootstrap requires a fresh locked
        // world read (or an already-known active town).
        if requested_at.is_none()
            && !self
                .towns
                .iter()
                .any(|town| town.id == projection.town.id && town.mayor_player_id == game_id)
        {
            return;
        }
        if let Some(pending) = self.pending_construction.get(&game_id) {
            let resolved = match requested_at {
                Some(started) => started > pending.started,
                None => projection.revision > pending.base_revision,
            };
            if !resolved {
                return;
            }
        }
        if projection.authority.as_deref() == Some(server_id) {
            // Reconcile the durable projection, including removals. Reservations
            // disappear only after a locked authoritative read or a newer commit.
            let wanted: HashSet<_> = projection
                .buildings
                .iter()
                .map(|record| record.building.id)
                .collect();
            if let Some(ids) = self.durable_buildings.remove(&game_id) {
                for id in ids {
                    if !wanted.contains(&id) {
                        self.buildings.remove(id);
                    }
                }
            }
            if let Some(pending) = self.pending_construction.remove(&game_id) {
                if let Some(id) = pending.reservation {
                    if !wanted.contains(&id) {
                        self.buildings.remove(id);
                    }
                }
                if let Some(player) = self.players.get_mut(&game_id) {
                    player.economy_pending = false;
                }
            }
            if !self.buildings.claim(&projection.town) {
                return;
            }
            self.towns.retain(|town| town.id != projection.town.id);
            self.towns.push(projection.town.clone());
            let mut ids = HashSet::new();
            for record in &projection.buildings {
                if let Some(existing) = self.buildings.buildings.get(&record.building.id) {
                    if existing.building_type == record.building.building_type
                        && existing.origin_tile == record.building.origin_tile
                        && existing.ownership == record.building.ownership
                    {
                        ids.insert(record.building.id);
                        continue;
                    }
                }
                match self.insert_building(record.building.clone()) {
                    Ok(()) => {
                        ids.insert(record.building.id);
                    }
                    Err(error) => {
                        tracing_error(error);
                    }
                }
            }
            self.durable_buildings.insert(game_id, ids);
        } else if projection.authority.is_none() {
            // A rolled-back very first command may not have initialized the row.
            if let Some(pending) = self.pending_construction.remove(&game_id) {
                if let Some(id) = pending.reservation {
                    self.buildings.remove(id);
                }
                if let Some(player) = self.players.get_mut(&game_id) {
                    player.economy_pending = false;
                }
            }
        }
        if let Some(player) = self.players.get_mut(&game_id) {
            player.game.wallet.cash = projection.cash;
        }
        self.economy_revisions.insert(game_id, projection.revision);
    }

    pub(super) fn prepare_construction(
        &mut self,
        preparation: ConstructionPreparation,
        server_id: &str,
    ) -> Result<(), ConstructionError> {
        let game_id = preparation.projection.town.mayor_player_id;
        if preparation.projection.authority.as_deref() != Some(server_id) {
            return Err(ConstructionError::AuthorityUnavailable);
        }
        if self.ended_games.contains(&game_id) {
            return Err(ConstructionError::InactiveGame);
        }
        // This projection was read under the row lock for the NEW operation, so
        // it can also settle an older command whose immediate apply was lost.
        self.apply_economy(
            preparation.projection.clone(),
            server_id,
            Some(Instant::now()),
        );
        if self.pending_construction.contains_key(&game_id) {
            return Err(ConstructionError::Busy);
        }
        let player = self
            .players
            .get(&game_id)
            .ok_or(ConstructionError::InactiveGame)?;
        if player.game.identity.role != Some(PlayerRole::Mayor)
            || player.game.identity.town_id.as_deref() != Some(&preparation.projection.town.id)
        {
            return Err(ConstructionError::NotMayor);
        }
        if player.hp <= 0 {
            return Err(ConstructionError::NotAlive);
        }
        if player.transferring {
            return Err(ConstructionError::Busy);
        }
        let territory = TownTerritory::for_town(&preparation.projection.town);
        if !territory.contains_point(player.x, player.y) {
            return Err(ConstructionError::OutsideTerritory);
        }
        let mut reservation = None;
        match &preparation.command {
            ConstructionCommand::PlaceBuilding {
                building_type,
                origin_tile,
                ..
            } => {
                let candidate = preparation
                    .candidate
                    .as_ref()
                    .ok_or(ConstructionError::UnavailableBuilding)?;
                if candidate.building.building_type != *building_type
                    || candidate.building.origin_tile != *origin_tile
                {
                    return Err(ConstructionError::UnavailableBuilding);
                }
                let geometry = building_type.definition().geometry;
                if !territory.contains_footprint(*origin_tile, geometry.footprint_tiles) {
                    return Err(ConstructionError::OutsideTerritory);
                }
                let collider = geometry.collider(*origin_tile);
                if self.players.values().any(|player| {
                    player.hp > 0 && circle_intersects(collider, player.x, player.y, PLAYER_RADIUS)
                }) {
                    return Err(ConstructionError::PlayerObstruction);
                }
                self.insert_building(candidate.building.clone())
                    .map_err(|error| {
                        if error.contains("ground") {
                            ConstructionError::InvalidTerrain
                        } else {
                            ConstructionError::Occupied
                        }
                    })?;
                reservation = Some(candidate.building.id);
            }
            ConstructionCommand::DemolishBuilding { building_id, .. } => {
                let building = self
                    .buildings
                    .buildings
                    .get(building_id)
                    .ok_or(ConstructionError::NotOwned)?;
                if building.building_type == BuildingType::TownHall {
                    return Err(ConstructionError::TownHallProtected);
                }
                if !building
                    .ownership
                    .as_ref()
                    .is_some_and(|owner| owner.game_id == game_id)
                {
                    return Err(ConstructionError::NotOwned);
                }
            }
        }
        self.pending_construction.insert(
            game_id,
            PendingConstruction {
                started: Instant::now(),
                base_revision: preparation.projection.revision,
                reservation,
            },
        );
        // Keep the validated actor alive/stationary until commit or rollback has
        // been reconciled. The reserved collider keeps other players out.
        self.players.get_mut(&game_id).unwrap().economy_pending = true;
        Ok(())
    }
}
fn circle_intersects(rect: WorldRect, x: f32, y: f32, radius: f32) -> bool {
    let dx = x - x.clamp(rect.x, rect.x + rect.width);
    let dy = y - y.clamp(rect.y, rect.y + rect.height);
    dx * dx + dy * dy < radius * radius
}
fn tracing_error(error: &str) {
    eprintln!("durable building projection failed: {error}");
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (
        World,
        EconomyProjection,
        ConstructionCommand,
        PurchasedBuilding,
    ) {
        let town = Town {
            id: "town".into(),
            name: "Town".into(),
            hall: hall_for_slot(0),
            created_at_ms: 1,
            mayor_player_id: Uuid::new_v4(),
        };
        let mut world = World::default();
        world.attach_player(
            town.mayor_player_id,
            Some(town.clone()),
            GameIdentity::mayor(town.mayor_player_id, town.id.clone()),
        );
        let state = EconomyState {
            cash: 100000,
            authority: Some("A".into()),
            ..Default::default()
        };
        let command = ConstructionCommand::PlaceBuilding {
            request_id: Uuid::new_v4(),
            building_type: BuildingType::WallResearch,
            origin_tile: IVec2 { x: 13, y: 16 },
        };
        let candidate = state.plan(&town, &command).unwrap().0.unwrap();
        (
            world,
            EconomyProjection::new(town, &state),
            command,
            candidate,
        )
    }
    #[test]
    fn reservation_survives_stale_reads_and_commit_rehydrates_after_cell_restart() {
        let (mut world, mut projection, command, candidate) = fixture();
        let id = projection.town.mayor_player_id;
        let old_read = Instant::now();
        world
            .prepare_construction(
                ConstructionPreparation {
                    command,
                    projection: projection.clone(),
                    candidate: Some(candidate.clone()),
                },
                "A",
            )
            .unwrap();
        assert!(world.players[&id].economy_pending);
        assert!(!world
            .visible_buildings(WorldRect {
                x: 0.0,
                y: 0.0,
                width: 1536.0,
                height: 1536.0
            })
            .iter()
            .any(|b| b.instance.id == candidate.building.id));
        world.apply_economy(projection.clone(), "A", Some(old_read));
        assert!(world.pending_construction.contains_key(&id));
        projection.revision = 1;
        projection.cash -= candidate.price_paid;
        projection.buildings.push(candidate.clone());
        world.apply_economy(projection.clone(), "A", None);
        assert!(!world.players[&id].economy_pending);
        assert_eq!(world.players[&id].game.wallet.cash, 95000);
        let mut restarted = World::default();
        restarted.apply_economy(projection.clone(), "A", None);
        assert!(
            !restarted
                .buildings
                .buildings
                .contains_key(&candidate.building.id),
            "an unsolicited old notification cannot bootstrap a town"
        );
        restarted.apply_economy(projection.clone(), "A", Some(Instant::now()));
        assert!(restarted
            .buildings
            .buildings
            .contains_key(&candidate.building.id));
        restarted.end_game(id);
        restarted.apply_economy(projection, "A", None);
        assert!(!restarted
            .buildings
            .buildings
            .contains_key(&candidate.building.id));
    }
    #[test]
    fn rollback_releases_reservation_without_placing_or_spending() {
        let (mut world, projection, command, candidate) = fixture();
        let id = projection.town.mayor_player_id;
        world
            .prepare_construction(
                ConstructionPreparation {
                    command,
                    projection: projection.clone(),
                    candidate: Some(candidate.clone()),
                },
                "A",
            )
            .unwrap();
        world.apply_economy(projection, "A", Some(Instant::now()));
        assert!(!world
            .buildings
            .buildings
            .contains_key(&candidate.building.id));
        assert!(!world.players[&id].economy_pending);
        assert_eq!(world.players[&id].game.wallet.cash, 100000);
    }
    #[test]
    fn authoritative_actor_and_player_overlap_are_rechecked() {
        let (mut world, projection, command, candidate) = fixture();
        let id = projection.town.mayor_player_id;
        let prepare = ConstructionPreparation {
            command,
            projection,
            candidate: Some(candidate),
        };
        world.players.get_mut(&id).unwrap().hp = 0;
        assert_eq!(
            world.prepare_construction(prepare.clone(), "A"),
            Err(ConstructionError::NotAlive)
        );
        world.players.get_mut(&id).unwrap().hp = 100;
        world.players.get_mut(&id).unwrap().x = -1.0;
        assert_eq!(
            world.prepare_construction(prepare.clone(), "A"),
            Err(ConstructionError::OutsideTerritory)
        );
        world.players.get_mut(&id).unwrap().x = 17.0 * TILE_PX;
        let other = Uuid::new_v4();
        let mut player = Player::new(other, GameIdentity::debug(other), None);
        player.x = 14.0 * TILE_PX;
        player.y = 17.0 * TILE_PX;
        world.players.insert(other, player);
        assert_eq!(
            world.prepare_construction(prepare, "A"),
            Err(ConstructionError::PlayerObstruction)
        );
    }

    #[test]
    fn occupied_forecourt_is_valid_but_reserved_footprints_cannot_overlap() {
        let (mut world, mut projection, _, research) = fixture();
        projection.buildings.push(research);
        let state = EconomyState {
            cash: 100000,
            buildings: projection.buildings.clone(),
            ..Default::default()
        };
        let command = ConstructionCommand::PlaceBuilding {
            request_id: Uuid::new_v4(),
            building_type: BuildingType::WallFactory,
            origin_tile: IVec2 { x: 16, y: 13 },
        };
        let candidate = state.plan(&projection.town, &command).unwrap().0.unwrap();
        let other = Uuid::new_v4();
        let mut player = Player::new(other, GameIdentity::debug(other), None);
        player.x = 17.0 * TILE_PX;
        player.y = 15.5 * TILE_PX;
        world.players.insert(other, player);
        assert!(world
            .prepare_construction(
                ConstructionPreparation {
                    command,
                    projection: projection.clone(),
                    candidate: Some(candidate)
                },
                "A"
            )
            .is_ok());
        // A new locked read settles this reservation as rolled back.
        world.apply_economy(projection.clone(), "A", Some(Instant::now()));
        let command = ConstructionCommand::PlaceBuilding {
            request_id: Uuid::new_v4(),
            building_type: BuildingType::WallResearch,
            origin_tile: IVec2 { x: 13, y: 16 },
        };
        let candidate = state.plan(&projection.town, &command).unwrap().0.unwrap();
        assert_eq!(
            world.prepare_construction(
                ConstructionPreparation {
                    command,
                    projection,
                    candidate: Some(candidate)
                },
                "A"
            ),
            Err(ConstructionError::Occupied)
        );
    }
}
