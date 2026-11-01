use super::{BuildingType, EntityId, GameId, ItemBuildingType, ItemType, TownId, INVENTORY_SLOTS};
use crate::{ClientId, IVec2};
use serde::{Deserialize, Serialize};

/// Explicit lifecycle owner, separate from the item's manufacturing origin.
/// Future transfers can change ownership without erasing provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityOwnership {
    pub game_id: GameId,
    pub town_id: Option<TownId>,
    pub owner_player_id: Option<ClientId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemInstance {
    pub id: EntityId,
    pub item_type: ItemType,
    pub ownership: EntityOwnership,
    pub source_building_id: Option<EntityId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
    pub slots: [Option<ItemInstance>; INVENTORY_SLOTS],
}

impl Default for Inventory {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
        }
    }
}

impl Inventory {
    pub fn clear(&mut self) {
        self.slots.fill(None);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    pub current: u32,
    pub maximum: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProductionState {
    Producing { ready_at_ms: u64 },
    OutputReady { item: ItemInstance },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildingInstance {
    pub id: EntityId,
    pub building_type: BuildingType,
    /// None is a physical neutral hall, not an unallocated player session.
    pub ownership: Option<EntityOwnership>,
    pub origin_tile: IVec2,
    pub health: Health,
    pub production: Option<ProductionState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildingView {
    #[serde(flatten)]
    pub instance: BuildingInstance,
    pub geometry: super::GeometryDefinition,
    pub town_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WorldPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeployedState {
    Wall,
    Turret {
        target_player_id: Option<ClientId>,
        next_fire_at_ms: u64,
    },
    Orb {
        target_hall_id: EntityId,
        detonate_at_ms: u64,
    },
    HomingMissile {
        target_player_id: Option<ClientId>,
        launched_at_ms: u64,
        initial_position: WorldPoint,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemBuildingInstance {
    pub id: EntityId,
    pub item_building_type: ItemBuildingType,
    pub ownership: EntityOwnership,
    pub position: WorldPoint,
    pub angle_radians: f32,
    pub health: Health,
    pub behavior: DeployedState,
}
