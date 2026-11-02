use std::collections::{HashMap, HashSet};
use twz_shared::{
    hall_entity_id, BuildingInstance, BuildingType, BuildingView, EntityId, EntityOwnership,
    GameId, Health, IVec2, ItemBuildingInstance, ItemInstance, Town, WorldRect, CELL_TILES,
    TILE_PX, TOWN_GRID, TOWN_OFFSET,
};

#[derive(Default, Clone)]
pub(crate) struct BuildingStore {
    pub buildings: HashMap<EntityId, BuildingInstance>,
    pub items: HashMap<EntityId, ItemInstance>,
    pub deployments: HashMap<EntityId, ItemBuildingInstance>,
    occupancy: HashMap<(i32, i32), EntityId>,
    chunks: HashSet<(i32, i32)>,
    names: HashMap<EntityId, String>,
    // Retain the last generation even when a hall becomes neutral, so a late
    // initial attachment cannot resurrect an ended claim or replace a newer one.
    claims: HashMap<EntityId, (i64, GameId, bool)>,
}

impl BuildingStore {
    pub fn ensure_chunk(&mut self, cx: i32, cy: i32) {
        if !self.chunks.insert((cx, cy)) {
            return;
        }
        for y in (TOWN_OFFSET..CELL_TILES).step_by(TOWN_GRID as usize) {
            for x in (TOWN_OFFSET..CELL_TILES).step_by(TOWN_GRID as usize) {
                let origin = IVec2 {
                    x: cx * CELL_TILES + x,
                    y: cy * CELL_TILES + y,
                };
                let id = hall_entity_id(origin);
                if !self.buildings.contains_key(&id) {
                    let maximum = BuildingType::TownHall.definition().max_health;
                    self.insert(
                        BuildingInstance {
                            id,
                            building_type: BuildingType::TownHall,
                            ownership: None,
                            origin_tile: origin,
                            health: Health {
                                current: maximum,
                                maximum,
                            },
                            production: None,
                        },
                        |_, _| true,
                    )
                    .expect("procedural hall footprints must not overlap");
                }
            }
        }
    }

    pub fn insert(
        &mut self,
        building: BuildingInstance,
        ground: impl Fn(i32, i32) -> bool,
    ) -> Result<(), &'static str> {
        if self.buildings.contains_key(&building.id) {
            return Err("building id already exists");
        }
        let geometry = building.building_type.definition().geometry;
        let mut tiles = Vec::new();
        for y in 0..geometry.footprint_tiles.y {
            for x in 0..geometry.footprint_tiles.x {
                let tile = (building.origin_tile.x + x, building.origin_tile.y + y);
                if !ground(tile.0, tile.1) {
                    return Err("building footprint requires ground");
                }
                if self.occupancy.contains_key(&tile) {
                    return Err("building footprint is occupied");
                }
                tiles.push(tile);
            }
        }
        for tile in tiles {
            self.occupancy.insert(tile, building.id);
        }
        self.buildings.insert(building.id, building);
        Ok(())
    }

    pub fn remove(&mut self, id: EntityId) {
        self.buildings.remove(&id);
        self.names.remove(&id);
        self.occupancy.retain(|_, owner| *owner != id);
        self.items
            .retain(|_, item| item.source_building_id != Some(id));
    }

    pub fn remove_game(&mut self, game_id: GameId) {
        let owned: Vec<_> = self
            .buildings
            .iter()
            .filter(|(_, b)| b.ownership.as_ref().is_some_and(|o| o.game_id == game_id))
            .map(|(id, _)| *id)
            .collect();
        for id in owned {
            if self.buildings[&id].building_type == BuildingType::TownHall {
                let hall = self.buildings.get_mut(&id).unwrap();
                hall.ownership = None;
                hall.health.current = hall.health.maximum;
                hall.production = None;
                if let Some(claim) = self.claims.get_mut(&id) {
                    claim.2 = true;
                }
                self.names.remove(&id);
            } else {
                self.remove(id);
            }
        }
        self.items
            .retain(|_, item| item.ownership.game_id != game_id);
        self.deployments
            .retain(|_, item| item.ownership.game_id != game_id);
    }

    pub fn reconcile(&mut self, towns: &[Town]) {
        let active: HashSet<_> = towns.iter().map(|town| town.mayor_player_id).collect();
        let expired: HashSet<_> = self
            .buildings
            .values()
            .filter_map(|b| b.ownership.as_ref())
            .chain(self.items.values().map(|i| &i.ownership))
            .chain(self.deployments.values().map(|i| &i.ownership))
            .filter(|o| !active.contains(&o.game_id))
            .map(|o| o.game_id)
            .collect();
        for game in expired {
            self.remove_game(game);
        }
        for town in towns {
            self.claim(town);
        }
    }

    pub fn claim(&mut self, town: &Town) -> bool {
        self.ensure_chunk(town.hall.cell.0, town.hall.cell.1);
        let id = hall_entity_id(town.hall.origin);
        if let Some((created, game, retired)) = self.claims.get(&id) {
            if town.created_at_ms < *created
                || (town.created_at_ms == *created && (*game != town.mayor_player_id || *retired))
            {
                return false;
            }
        }
        if let Some(old) = self.buildings.get(&id).and_then(|b| b.ownership.as_ref()) {
            if old.game_id != town.mayor_player_id {
                self.remove_game(old.game_id);
            }
        }
        if let Some(hall) = self.buildings.get_mut(&id) {
            hall.ownership = Some(EntityOwnership {
                game_id: town.mayor_player_id,
                town_id: Some(town.id.clone()),
                owner_player_id: Some(town.mayor_player_id),
            });
            self.names.insert(id, town.name.clone());
            self.claims
                .insert(id, (town.created_at_ms, town.mayor_player_id, false));
        }
        true
    }

    fn candidates(&self, bounds: WorldRect) -> HashSet<EntityId> {
        let mut ids = HashSet::new();
        for y in (bounds.y / TILE_PX).floor() as i32
            ..=((bounds.y + bounds.height) / TILE_PX).floor() as i32
        {
            for x in (bounds.x / TILE_PX).floor() as i32
                ..=((bounds.x + bounds.width) / TILE_PX).floor() as i32
            {
                if let Some(id) = self.occupancy.get(&(x, y)) {
                    ids.insert(*id);
                }
            }
        }
        ids
    }

    pub fn push_circle(&self, x: &mut f32, y: &mut f32, radius: f32) {
        for _ in 0..8 {
            let ids = self.candidates(WorldRect {
                x: *x - radius,
                y: *y - radius,
                width: 2.0 * radius,
                height: 2.0 * radius,
            });
            let mut pushed = false;
            for id in ids {
                let b = &self.buildings[&id];
                pushed |= b
                    .building_type
                    .definition()
                    .geometry
                    .collider(b.origin_tile)
                    .push_circle(x, y, radius);
            }
            if !pushed {
                break;
            }
        }
    }

    pub fn blocks_segment(&self, from: (f32, f32), to: (f32, f32), town_id: Option<&str>) -> bool {
        let bounds = WorldRect {
            x: from.0.min(to.0),
            y: from.1.min(to.1),
            width: (to.0 - from.0).abs(),
            height: (to.1 - from.1).abs(),
        };
        self.candidates(bounds).into_iter().any(|id| {
            let b = &self.buildings[&id];
            let friendly = town_id.is_some()
                && b.ownership.as_ref().and_then(|o| o.town_id.as_deref()) == town_id;
            !friendly
                && b.building_type
                    .definition()
                    .geometry
                    .collider(b.origin_tile)
                    .intersects_segment(from, to)
        })
    }

    pub fn views(&self, bounds: WorldRect) -> Vec<BuildingView> {
        self.candidates(bounds)
            .into_iter()
            .map(|id| {
                let b = &self.buildings[&id];
                BuildingView {
                    instance: b.clone(),
                    geometry: b.building_type.definition().geometry,
                    town_name: self.names.get(&id).cloned(),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn footprint_reserves_forecourt_and_rejects_partial_water_atomically() {
        let mut store = BuildingStore::default();
        store.ensure_chunk(0, 0);
        let b = BuildingInstance {
            id: uuid::Uuid::new_v4(),
            building_type: BuildingType::WallResearch,
            ownership: None,
            origin_tile: IVec2 { x: 16, y: 18 },
            health: Health {
                current: 255,
                maximum: 255,
            },
            production: None,
        };
        assert!(
            store.insert(b.clone(), |_, _| true).is_err(),
            "forecourt remains reserved"
        );
        let mut b = b;
        b.origin_tile = IVec2 { x: 24, y: 24 };
        assert!(store.insert(b.clone(), |x, y| x != 26 || y != 26).is_err());
        assert!(store.insert(b.clone(), |_, _| true).is_ok());
        store.remove(b.id);
        assert!(store.insert(b, |_, _| true).is_ok());
    }
    #[test]
    fn claim_cleanup_and_reclaim_keep_one_physical_hall() {
        let mut store = BuildingStore::default();
        store.ensure_chunk(0, 0);
        let mut town = Town {
            id: "a".into(),
            name: "First".into(),
            hall: twz_shared::hall_for_slot(0),
            created_at_ms: 1,
            mayor_player_id: uuid::Uuid::new_v4(),
        };
        let old = town.mayor_player_id;
        store.claim(&town);
        let path = ((700.0, 800.0), (1000.0, 800.0));
        assert!(!store.blocks_segment(path.0, path.1, Some("a")));
        assert!(store.blocks_segment(path.0, path.1, Some("enemy")));
        store.remove_game(old);
        assert_eq!(store.buildings.len(), 4);
        assert!(store.blocks_segment(path.0, path.1, None));
        let stale = town.clone();
        town.id = "b".into();
        town.created_at_ms += 1;
        town.mayor_player_id = uuid::Uuid::new_v4();
        store.claim(&town);
        assert!(
            !store.claim(&stale),
            "delayed old claim cannot replace the new game"
        );
        store.remove_game(old);
        assert_eq!(
            store.buildings[&hall_entity_id(town.hall.origin)]
                .ownership
                .as_ref()
                .unwrap()
                .game_id,
            town.mayor_player_id
        );
    }

    #[test]
    fn negative_chunks_and_forecourt_collision_agree_with_occupancy() {
        let mut store = BuildingStore::default();
        store.ensure_chunk(-1, -1);
        assert_eq!(store.buildings.len(), 4);
        let origin = IVec2 { x: -16, y: -16 };
        assert!(store.buildings.contains_key(&hall_entity_id(origin)));
        let (mut x, mut y) = (-696.0, -648.0);
        store.push_circle(&mut x, &mut y, 16.0);
        assert_eq!((x, y), (-696.0, -648.0), "walkable bottom row");
        y = -730.0;
        store.push_circle(&mut x, &mut y, 16.0);
        assert_ne!(y, -730.0, "solid body ejects an embedded circle");
    }
}
