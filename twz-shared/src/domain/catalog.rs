use serde::{Deserialize, Serialize};
use crate::IVec2;
use super::{PlayerRole, DOMAIN_VERSION};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildingType {
    TownHall, Hospital, House, LaserResearch, LaserFactory,
    TurretResearch, TurretFactory, TimeBombResearch, TimeBombFactory,
    MedKitResearch, PlasmaTurretResearch, PlasmaTurretFactory,
    MineResearch, MineFactory, OrbResearch, OrbFactory,
    BazookaResearch, BazookaFactory, SleeperResearch, SleeperFactory,
    WallResearch, WallFactory, DfgResearch, DfgFactory,
    FlareGunResearch, FlareGunFactory, HomingMissileResearch, HomingMissileFactory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    Cloak, Rocket, MedKit, Bomb, Mine, Orb, Flare, Dfg, Wall, Turret, Sleeper, Plasma,
    HomingMissile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemBuildingType { Wall, Turret, Orb, HomingMissile }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DefinitionStatus {
    /// Sufficient legacy rules for the planned branch, not enabled gameplay.
    Specified,
    /// Known ID/properties only; no gameplay behavior is invented.
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelRect { pub x: u16, pub y: u16, pub width: u16, pub height: u16 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryDefinition {
    /// Top-left tile anchor. Reserved placement area can exceed the collider.
    pub footprint_tiles: IVec2,
    pub collision_px: PixelRect,
    /// Sprite geometry is unspecified until art/placement work in Phase 2.
    pub visual_bounds_px: Option<PixelRect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProductionRecipe {
    pub output: ItemType,
    pub duration_ms: Option<u64>,
    pub output_capacity: u8,
    pub pickup_offset_px: IVec2,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct BuildingDefinition {
    pub building_type: BuildingType,
    pub name: &'static str,
    pub cost: u64,
    pub prerequisite: Option<BuildingType>,
    pub status: DefinitionStatus,
    pub max_health: u32,
    pub destructible: bool,
    pub geometry: GeometryDefinition,
    pub max_distance_from_hall_px: u32,
    pub production: Option<ProductionRecipe>,
}

const fn recipe(output: ItemType, duration_ms: Option<u64>) -> Option<ProductionRecipe> {
    Some(ProductionRecipe { output, duration_ms, output_capacity: 1, pickup_offset_px: IVec2 { x: 48, y: 96 } })
}

const fn building(
    building_type: BuildingType, name: &'static str, cost: u64,
    prerequisite: Option<BuildingType>, status: DefinitionStatus,
    production: Option<ProductionRecipe>,
) -> BuildingDefinition {
    let short = matches!(building_type, BuildingType::TownHall | BuildingType::WallFactory
        | BuildingType::TurretFactory | BuildingType::OrbFactory | BuildingType::HomingMissileFactory);
    BuildingDefinition {
        building_type, name, cost, prerequisite, status,
        max_health: if matches!(building_type, BuildingType::TownHall) { 200 } else { 255 },
        destructible: !matches!(building_type, BuildingType::TownHall),
        geometry: GeometryDefinition {
            footprint_tiles: IVec2 { x: 3, y: 3 },
            collision_px: PixelRect { x: 0, y: 0, width: 144, height: if short { 96 } else { 144 } },
            visual_bounds_px: Some(PixelRect { x: 0, y: 0, width: 144, height: 144 }),
        },
        max_distance_from_hall_px: 1500,
        production,
    }
}

use BuildingType as B;
use DefinitionStatus::{Specified as S, Unavailable as U};
use ItemType as I;

/// Legacy prices, known prerequisites and production data. Presence of the
/// research building gates factory placement; this is not a permanent unlock.
pub const BUILDING_DEFINITIONS: &[BuildingDefinition] = &[
    building(B::TownHall, "Town Hall", 0, None, S, None),
    building(B::Hospital, "Hospital", 5000, None, U, None),
    building(B::House, "House", 1000, None, U, None),
    building(B::LaserResearch, "Laser Research", 2000, None, U, None),
    building(B::LaserFactory, "Laser Factory", 20000, Some(B::LaserResearch), U, None),
    building(B::TurretResearch, "Turret Research", 5000, None, S, None),
    building(B::TurretFactory, "Turret Factory", 50000, Some(B::TurretResearch), S, recipe(I::Turret, Some(30000))),
    building(B::TimeBombResearch, "Time Bomb Research", 10000, None, U, None),
    building(B::TimeBombFactory, "Time Bomb Factory", 100000, Some(B::TimeBombResearch), U, recipe(I::Bomb, None)),
    building(B::MedKitResearch, "Med Kit Research", 5000, None, U, None),
    building(B::PlasmaTurretResearch, "Plasma Turret Research", 15000, None, U, None),
    building(B::PlasmaTurretFactory, "Plasma Turret Factory", 150000, Some(B::PlasmaTurretResearch), U, recipe(I::Plasma, None)),
    building(B::MineResearch, "Mine Research", 10000, None, U, None),
    building(B::MineFactory, "Mine Factory", 100000, Some(B::MineResearch), U, recipe(I::Mine, None)),
    building(B::OrbResearch, "Orb Research", 20000, None, S, None),
    building(B::OrbFactory, "Orb Factory", 200000, Some(B::OrbResearch), S, recipe(I::Orb, Some(360000))),
    building(B::BazookaResearch, "Bazooka Research", 50000, None, U, None),
    building(B::BazookaFactory, "Bazooka Factory", 500000, Some(B::BazookaResearch), U, recipe(I::Rocket, None)),
    building(B::SleeperResearch, "Sleeper Research", 40000, None, U, None),
    building(B::SleeperFactory, "Sleeper Factory", 400000, Some(B::SleeperResearch), U, recipe(I::Sleeper, None)),
    building(B::WallResearch, "Wall Research", 5000, None, S, None),
    building(B::WallFactory, "Wall Factory", 50000, Some(B::WallResearch), S, recipe(I::Wall, Some(30000))),
    building(B::DfgResearch, "DFG Research", 500000, None, U, None),
    building(B::DfgFactory, "DFG Factory", 5000000, Some(B::DfgResearch), U, recipe(I::Dfg, None)),
    building(B::FlareGunResearch, "Flare Gun Research", 20000, None, U, None),
    building(B::FlareGunFactory, "Flare Gun Factory", 200000, Some(B::FlareGunResearch), U, recipe(I::Flare, None)),
    building(B::HomingMissileResearch, "Homing Missile Research", 20000, None, S, None),
    building(B::HomingMissileFactory, "Homing Missile Factory", 200000, Some(B::HomingMissileResearch), S, recipe(I::HomingMissile, Some(60000))),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemUse {
    Deploy { deployment: ItemBuildingType },
    Unspecified,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ItemDefinition {
    pub item_type: ItemType,
    pub name: &'static str,
    pub status: DefinitionStatus,
    pub usage: ItemUse,
    pub pickup_roles: &'static [PlayerRole],
}

const fn item(item_type: I, name: &'static str, status: DefinitionStatus, usage: ItemUse, pickup_roles: &'static [PlayerRole]) -> ItemDefinition {
    ItemDefinition { item_type, name, status, usage, pickup_roles }
}

pub const ITEM_DEFINITIONS: &[ItemDefinition] = &[
    item(I::Cloak, "Cloak", U, ItemUse::Unspecified, &[PlayerRole::Mayor]),
    item(I::Rocket, "Rocket", U, ItemUse::Unspecified, &[PlayerRole::Mayor, PlayerRole::Commando]),
    item(I::MedKit, "Med Kit", U, ItemUse::Unspecified, &[PlayerRole::Mayor]),
    item(I::Bomb, "Bomb", U, ItemUse::Unspecified, &[PlayerRole::Mayor]),
    item(I::Mine, "Mine", U, ItemUse::Unspecified, &[PlayerRole::Mayor]),
    item(I::Orb, "Orb", S, ItemUse::Deploy { deployment: ItemBuildingType::Orb }, &[PlayerRole::Mayor, PlayerRole::Commando]),
    item(I::Flare, "Flare", U, ItemUse::Unspecified, &[PlayerRole::Mayor]),
    item(I::Dfg, "DFG", U, ItemUse::Unspecified, &[PlayerRole::Mayor]),
    item(I::Wall, "Wall", S, ItemUse::Deploy { deployment: ItemBuildingType::Wall }, &[PlayerRole::Mayor]),
    item(I::Turret, "Turret", S, ItemUse::Deploy { deployment: ItemBuildingType::Turret }, &[PlayerRole::Mayor]),
    item(I::Sleeper, "Sleeper", U, ItemUse::Unspecified, &[PlayerRole::Mayor]),
    item(I::Plasma, "Plasma", U, ItemUse::Unspecified, &[PlayerRole::Mayor]),
    item(I::HomingMissile, "Homing Missile", S, ItemUse::Deploy { deployment: ItemBuildingType::HomingMissile }, &[PlayerRole::Mayor, PlayerRole::Commando]),
];

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeploymentBehavior {
    Wall,
    Turret { fire_interval_ms: u64, idle_turn_interval_ms: u64, shot_damage: u32, shot_range_px: u32, legacy_shot_speed_per_tick: u32, legacy_tick_ms: u32 },
    Orb { detonation_delay_ms: u64, enemy_hall_reach_px: u32 },
    HomingMissile { damage: u32, maximum_travel_px: u32, legacy_min_speed_per_tick: u32, legacy_max_speed_per_tick: u32, legacy_tick_ms: u32 },
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetingRule {
    None,
    NearestEnemyPlayerInInterestArea,
    EnemyTownHall,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ItemBuildingDefinition {
    pub item_building_type: ItemBuildingType,
    pub max_health: u32,
    pub destructible: bool,
    /// The legacy client supplied deployment sizes. Do not silently invent
    /// canonical geometry; specify it during the placement/geometry phase.
    pub geometry: Option<GeometryDefinition>,
    pub targeting: TargetingRule,
    pub max_distance_from_hall_px: Option<u32>,
    pub behavior: DeploymentBehavior,
}

pub const ITEM_BUILDING_DEFINITIONS: &[ItemBuildingDefinition] = &[
    ItemBuildingDefinition { item_building_type: ItemBuildingType::Wall, max_health: 100, destructible: true, geometry: None, targeting: TargetingRule::None, max_distance_from_hall_px: Some(1500), behavior: DeploymentBehavior::Wall },
    ItemBuildingDefinition { item_building_type: ItemBuildingType::Turret, max_health: 100, destructible: true, geometry: None, targeting: TargetingRule::NearestEnemyPlayerInInterestArea, max_distance_from_hall_px: Some(1500), behavior: DeploymentBehavior::Turret { fire_interval_ms: 500, idle_turn_interval_ms: 250, shot_damage: 5, shot_range_px: 260, legacy_shot_speed_per_tick: 8, legacy_tick_ms: 15 } },
    ItemBuildingDefinition { item_building_type: ItemBuildingType::Orb, max_health: 100, destructible: true, geometry: None, targeting: TargetingRule::EnemyTownHall, max_distance_from_hall_px: None, behavior: DeploymentBehavior::Orb { detonation_delay_ms: 10000, enemy_hall_reach_px: 150 } },
    ItemBuildingDefinition { item_building_type: ItemBuildingType::HomingMissile, max_health: 30, destructible: true, geometry: None, targeting: TargetingRule::NearestEnemyPlayerInInterestArea, max_distance_from_hall_px: None, behavior: DeploymentBehavior::HomingMissile { damage: 20, maximum_travel_px: 1200, legacy_min_speed_per_tick: 1, legacy_max_speed_per_tick: 10, legacy_tick_ms: 15 } },
];

#[derive(Serialize)]
pub struct GameCatalog {
    pub version: u16,
    pub buildings: &'static [BuildingDefinition],
    pub items: &'static [ItemDefinition],
    pub item_buildings: &'static [ItemBuildingDefinition],
}

pub fn game_catalog() -> GameCatalog {
    GameCatalog { version: DOMAIN_VERSION, buildings: BUILDING_DEFINITIONS, items: ITEM_DEFINITIONS, item_buildings: ITEM_BUILDING_DEFINITIONS }
}

impl BuildingType {
    pub fn definition(self) -> &'static BuildingDefinition {
        BUILDING_DEFINITIONS.iter().find(|definition| definition.building_type == self)
            .expect("every building type has a catalog definition")
    }
}

impl ItemType {
    pub fn definition(self) -> &'static ItemDefinition {
        ITEM_DEFINITIONS.iter().find(|definition| definition.item_type == self)
            .expect("every item type has a catalog definition")
    }
}

impl ItemBuildingType {
    pub fn definition(self) -> &'static ItemBuildingDefinition {
        ITEM_BUILDING_DEFINITIONS.iter().find(|definition| definition.item_building_type == self)
            .expect("every deployment type has a catalog definition")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_ids_are_unique_and_references_resolve() {
        let mut ids = HashSet::new();
        for building in BUILDING_DEFINITIONS {
            assert!(ids.insert(serde_json::to_string(&building.building_type).unwrap()));
            if let Some(prerequisite) = building.prerequisite {
                assert!(BUILDING_DEFINITIONS.iter().any(|b| b.building_type == prerequisite));
            }
            if let Some(recipe) = building.production {
                let item = ITEM_DEFINITIONS.iter().find(|i| i.item_type == recipe.output).unwrap();
                if building.status == S {
                    assert!(recipe.duration_ms.is_some_and(|ms| ms > 0));
                    assert_eq!(item.status, S);
                }
            }
        }
        assert_eq!(ids.len(), 28);
        ids.clear();
        for item in ITEM_DEFINITIONS {
            assert!(ids.insert(serde_json::to_string(&item.item_type).unwrap()));
            if let ItemUse::Deploy { deployment } = item.usage {
                assert!(ITEM_BUILDING_DEFINITIONS.iter().any(|d| d.item_building_type == deployment));
            }
        }
        assert_eq!(ids.len(), 13);
    }

    #[test]
    fn rocket_and_homing_missile_are_distinct_and_unknown_content_is_unavailable() {
        let bazooka = BUILDING_DEFINITIONS.iter().find(|b| b.building_type == B::BazookaFactory).unwrap();
        let homing = BUILDING_DEFINITIONS.iter().find(|b| b.building_type == B::HomingMissileFactory).unwrap();
        assert_eq!(bazooka.production.unwrap().output, I::Rocket);
        assert_eq!(bazooka.status, U);
        assert_eq!(homing.production.unwrap().output, I::HomingMissile);
        assert_eq!(homing.production.unwrap().duration_ms, Some(60000));
        assert_eq!(serde_json::to_string(&I::HomingMissile).unwrap(), "\"homing_missile\"");
        for id in [B::Hospital, B::House, B::DfgFactory, B::LaserFactory] {
            assert_eq!(BUILDING_DEFINITIONS.iter().find(|b| b.building_type == id).unwrap().status, U);
        }
    }
}
