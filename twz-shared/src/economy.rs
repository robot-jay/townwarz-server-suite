//! Phase 3 economic intentions, durable results, and town territory contracts.
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SUBJ_ECONOMY: &str = "twz.economy.request";
pub const SUBJ_ECONOMY_WORLD: &str = "twz.economy.world";
pub fn subj_construction_prepare(server: &str) -> String {
    format!("twz.economy.{server}.prepare")
}
pub fn subj_economy_apply(server: &str) -> String {
    format!("twz.economy.{server}.apply")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TownTerritory {
    pub min_tile: IVec2,
    pub size_tiles: IVec2,
}
impl TownTerritory {
    pub fn for_town(town: &Town) -> Self {
        Self {
            min_tile: IVec2 {
                x: town.hall.origin.x.div_euclid(TOWN_GRID) * TOWN_GRID,
                y: town.hall.origin.y.div_euclid(TOWN_GRID) * TOWN_GRID,
            },
            size_tiles: IVec2 {
                x: TOWN_GRID,
                y: TOWN_GRID,
            },
        }
    }
    pub fn contains_point(&self, x: f32, y: f32) -> bool {
        x.is_finite()
            && y.is_finite()
            && x >= self.min_tile.x as f32 * TILE_PX
            && y >= self.min_tile.y as f32 * TILE_PX
            && x < (self.min_tile.x as i64 + self.size_tiles.x as i64) as f32 * TILE_PX
            && y < (self.min_tile.y as i64 + self.size_tiles.y as i64) as f32 * TILE_PX
    }
    pub fn contains_footprint(&self, origin: IVec2, size: IVec2) -> bool {
        origin.x >= self.min_tile.x
            && origin.y >= self.min_tile.y
            && size.x > 0
            && size.y > 0
            && origin.x as i64 + size.x as i64 <= self.min_tile.x as i64 + self.size_tiles.x as i64
            && origin.y as i64 + size.y as i64 <= self.min_tile.y as i64 + self.size_tiles.y as i64
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConstructionCommand {
    PlaceBuilding {
        request_id: ClientId,
        building_type: BuildingType,
        origin_tile: IVec2,
    },
    DemolishBuilding {
        request_id: ClientId,
        building_id: EntityId,
    },
}
impl ConstructionCommand {
    pub fn request_id(&self) -> ClientId {
        match self {
            Self::PlaceBuilding { request_id, .. } | Self::DemolishBuilding { request_id, .. } => {
                *request_id
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstructionError {
    InactiveGame,
    StaleConnection,
    NotMayor,
    NotAlive,
    OutsideTerritory,
    UnavailableBuilding,
    InsufficientCash,
    MissingResearch,
    InvalidTerrain,
    Occupied,
    PlayerObstruction,
    NotOwned,
    TownHallProtected,
    RequestIdConflict,
    AuthorityUnavailable,
    Busy,
    CashLimit,
    ServiceUnavailable,
    InvalidCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurchasedBuilding {
    pub building: BuildingInstance,
    pub price_paid: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstructionResult {
    pub request_id: ClientId,
    pub error: Option<ConstructionError>,
    pub building_id: Option<EntityId>,
    pub cash: u64,
    pub refund: u64,
}
impl ConstructionResult {
    pub fn rejected(command: &ConstructionCommand, error: ConstructionError, cash: u64) -> Self {
        Self {
            request_id: command.request_id(),
            error: Some(error),
            building_id: None,
            cash,
            refund: 0,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedConstruction {
    pub command: ConstructionCommand,
    pub result: ConstructionResult,
}

/// One durable row is the serialization boundary for a solo Mayor's economy.
/// Movement checkpoints may never replace this state once it exists.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EconomyState {
    pub cash: u64,
    pub income_progress_ms: u64,
    pub income_connection: Option<ClientId>,
    pub connection_elapsed_ms: u64,
    pub revision: u64,
    pub authority: Option<String>,
    pub buildings: Vec<PurchasedBuilding>,
    pub outcomes: BTreeMap<ClientId, RecordedConstruction>,
}

#[derive(Debug, Clone, Copy)]
pub struct IncomePolicy {
    pub amount: u64,
    pub interval_ms: u64,
}
impl Default for IncomePolicy {
    fn default() -> Self {
        Self {
            amount: 100,
            interval_ms: 3000,
        }
    }
}
impl EconomyState {
    /// Cumulative eligible time is supplied by the authenticated gateway session,
    /// not the browser or a cell avatar. Retries/handoffs cannot add time twice.
    pub fn accrue(&mut self, connection: ClientId, elapsed_ms: u64, policy: IncomePolicy) {
        if self.income_connection != Some(connection) {
            self.income_connection = Some(connection);
            self.connection_elapsed_ms = 0;
        }
        let delta = elapsed_ms.saturating_sub(self.connection_elapsed_ms);
        self.connection_elapsed_ms = self.connection_elapsed_ms.max(elapsed_ms);
        let total = self.income_progress_ms.saturating_add(delta);
        let interval = policy.interval_ms.max(1);
        let earned = (total / interval).saturating_mul(policy.amount);
        self.income_progress_ms = total % interval;
        self.cash = self.cash.saturating_add(earned).min(Wallet::MAX_CASH);
    }
    pub fn replay(&self, command: &ConstructionCommand) -> Option<ConstructionResult> {
        self.outcomes.get(&command.request_id()).map(|record| {
            if record.command == *command {
                record.result.clone()
            } else {
                ConstructionResult::rejected(
                    command,
                    ConstructionError::RequestIdConflict,
                    self.cash,
                )
            }
        })
    }
    /// Pure economic planning. Spatial validation/reservation happens on the cell
    /// before this candidate may be durably committed.
    pub fn plan(
        &self,
        town: &Town,
        command: &ConstructionCommand,
    ) -> Result<(Option<PurchasedBuilding>, u64), ConstructionError> {
        match command {
            ConstructionCommand::PlaceBuilding {
                building_type,
                origin_tile,
                ..
            } => {
                if !matches!(
                    building_type,
                    BuildingType::WallResearch | BuildingType::WallFactory
                ) {
                    return Err(if *building_type == BuildingType::TownHall {
                        ConstructionError::TownHallProtected
                    } else {
                        ConstructionError::UnavailableBuilding
                    });
                }
                let definition = building_type.definition();
                if !TownTerritory::for_town(town)
                    .contains_footprint(*origin_tile, definition.geometry.footprint_tiles)
                {
                    return Err(ConstructionError::OutsideTerritory);
                }
                if self.cash < definition.cost {
                    return Err(ConstructionError::InsufficientCash);
                }
                if definition.prerequisite.is_some_and(|required| {
                    !self
                        .buildings
                        .iter()
                        .any(|b| b.building.building_type == required)
                }) {
                    return Err(ConstructionError::MissingResearch);
                }
                Ok((
                    Some(PurchasedBuilding {
                        building: BuildingInstance {
                            // Independent from the client-selected request UUID.
                            id: ClientId::new_v4(),
                            building_type: *building_type,
                            ownership: Some(EntityOwnership {
                                game_id: town.mayor_player_id,
                                town_id: Some(town.id.clone()),
                                owner_player_id: Some(town.mayor_player_id),
                            }),
                            origin_tile: *origin_tile,
                            health: Health {
                                current: definition.max_health,
                                maximum: definition.max_health,
                            },
                            production: None,
                        },
                        price_paid: definition.cost,
                    }),
                    0,
                ))
            }
            ConstructionCommand::DemolishBuilding { building_id, .. } => {
                if *building_id == hall_entity_id(town.hall.origin) {
                    return Err(ConstructionError::TownHallProtected);
                }
                let building = self
                    .buildings
                    .iter()
                    .find(|b| b.building.id == *building_id)
                    .ok_or(ConstructionError::NotOwned)?;
                let refund = building.price_paid / 4;
                if self
                    .cash
                    .checked_add(refund)
                    .filter(|v| *v <= Wallet::MAX_CASH)
                    .is_none()
                {
                    return Err(ConstructionError::CashLimit);
                }
                Ok((None, refund))
            }
        }
    }
    pub fn apply_purchase(
        &mut self,
        command: &ConstructionCommand,
        building: Option<PurchasedBuilding>,
        refund: u64,
    ) -> ConstructionResult {
        let building_id = match command {
            ConstructionCommand::PlaceBuilding { .. } => {
                let building = building.expect("validated purchase");
                self.cash -= building.price_paid;
                let id = building.building.id;
                self.buildings.push(building);
                id
            }
            ConstructionCommand::DemolishBuilding { building_id, .. } => {
                self.buildings.retain(|b| b.building.id != *building_id);
                self.cash += refund;
                *building_id
            }
        };
        ConstructionResult {
            request_id: command.request_id(),
            error: None,
            building_id: Some(building_id),
            cash: self.cash,
            refund,
        }
    }
    pub fn record(&mut self, command: ConstructionCommand, result: ConstructionResult) {
        self.outcomes.insert(
            command.request_id(),
            RecordedConstruction { command, result },
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EconomyRequest {
    pub player_id: ClientId,
    pub connection_id: ClientId,
    pub elapsed_ms: u64,
    pub command: Option<ConstructionCommand>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EconomyProjection {
    pub town: Town,
    pub revision: u64,
    pub cash: u64,
    pub authority: Option<String>,
    pub buildings: Vec<PurchasedBuilding>,
}
impl EconomyProjection {
    pub fn new(town: Town, state: &EconomyState) -> Self {
        Self {
            town,
            revision: state.revision,
            cash: state.cash,
            authority: state.authority.clone(),
            buildings: state.buildings.clone(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EconomyResponse {
    pub error: Option<ConstructionError>,
    pub result: Option<ConstructionResult>,
    pub projection: Option<EconomyProjection>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstructionPreparation {
    pub command: ConstructionCommand,
    pub projection: EconomyProjection,
    pub candidate: Option<PurchasedBuilding>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstructionPrepared {
    pub error: Option<ConstructionError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstructionStatus {
    pub territory: Option<TownTerritory>,
    pub can_construct: bool,
    pub reason: Option<ConstructionError>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn town() -> Town {
        Town {
            id: "town".into(),
            name: "Town".into(),
            hall: hall_for_slot(0),
            created_at_ms: 1,
            mayor_player_id: ClientId::new_v4(),
        }
    }
    fn place(kind: BuildingType) -> ConstructionCommand {
        ConstructionCommand::PlaceBuilding {
            request_id: ClientId::new_v4(),
            building_type: kind,
            origin_tile: IVec2 { x: 13, y: 16 },
        }
    }
    #[test]
    fn income_is_eligible_time_not_reconnect_or_handoff_wall_time() {
        let mut state = EconomyState::default();
        let first = ClientId::new_v4();
        state.accrue(first, 1700, IncomePolicy::default());
        state.accrue(first, 1700, IncomePolicy::default());
        state.accrue(first, 100, IncomePolicy::default());
        assert_eq!((state.cash, state.income_progress_ms), (0, 1700));
        state.accrue(first, 2000, IncomePolicy::default());
        let resumed = ClientId::new_v4();
        state.accrue(resumed, 999, IncomePolicy::default());
        assert_eq!(state.cash, 0);
        state.accrue(resumed, 1000, IncomePolicy::default());
        assert_eq!((state.cash, state.income_progress_ms), (100, 0));
        state.accrue(resumed, 7000, IncomePolicy::default());
        assert_eq!((state.cash, state.income_progress_ms), (300, 0));
    }
    #[test]
    fn territories_include_the_whole_forecourt_and_handle_negative_coordinates() {
        let mut town = town();
        town.hall.origin = IVec2 { x: -16, y: -16 };
        let territory = TownTerritory::for_town(&town);
        assert_eq!(territory.min_tile, IVec2 { x: -32, y: -32 });
        assert!(territory.contains_footprint(IVec2 { x: -3, y: -3 }, IVec2 { x: 3, y: 3 }));
        assert!(!territory.contains_footprint(IVec2 { x: -2, y: -3 }, IVec2 { x: 3, y: 3 }));
        assert!(!territory.contains_footprint(IVec2 { x: i32::MAX, y: 0 }, IVec2 { x: 3, y: 3 }));
        assert!(!territory.contains_point(0.0, -100.0));
        assert!(!territory.contains_point(f32::NAN, -100.0));
    }
    #[test]
    fn research_gates_new_factories_and_refunds_use_original_price() {
        let town = town();
        let mut state = EconomyState {
            cash: 125000,
            ..Default::default()
        };
        let factory_command = place(BuildingType::WallFactory);
        assert_eq!(
            state.plan(&town, &factory_command).unwrap_err(),
            ConstructionError::MissingResearch
        );
        assert_eq!(
            state
                .plan(&town, &place(BuildingType::TurretResearch))
                .unwrap_err(),
            ConstructionError::UnavailableBuilding
        );
        let research_command = place(BuildingType::WallResearch);
        let (research, refund) = state.plan(&town, &research_command).unwrap();
        let result = state.apply_purchase(&research_command, research, refund);
        state.record(research_command.clone(), result.clone());
        assert_eq!(
            state.replay(&research_command).unwrap().building_id,
            result.building_id
        );
        let conflict = ConstructionCommand::DemolishBuilding {
            request_id: research_command.request_id(),
            building_id: result.building_id.unwrap(),
        };
        assert_eq!(
            state.replay(&conflict).unwrap().error,
            Some(ConstructionError::RequestIdConflict)
        );
        let (factory, refund) = state.plan(&town, &factory_command).unwrap();
        state.apply_purchase(&factory_command, factory, refund);
        // Provenance is distinct from today's catalog price and uses floor.
        state.buildings[0].price_paid = 5003;
        let demolish = ConstructionCommand::DemolishBuilding {
            request_id: ClientId::new_v4(),
            building_id: result.building_id.unwrap(),
        };
        let (_, refund) = state.plan(&town, &demolish).unwrap();
        assert_eq!(refund, 1250);
        state.apply_purchase(&demolish, None, refund);
        assert_eq!(state.buildings.len(), 1);
        assert_eq!(
            state.buildings[0].building.building_type,
            BuildingType::WallFactory
        );
        assert_eq!(
            state
                .plan(&town, &place(BuildingType::WallFactory))
                .unwrap_err(),
            ConstructionError::MissingResearch
        );
        state.cash = 0;
        assert_eq!(
            state
                .plan(&town, &place(BuildingType::WallResearch))
                .unwrap_err(),
            ConstructionError::InsufficientCash
        );
    }
    #[test]
    fn browser_commands_cannot_supply_economic_or_ownership_fields() {
        let mut value = serde_json::to_value(place(BuildingType::WallResearch)).unwrap();
        value["price"] = 0.into();
        assert!(serde_json::from_value::<ConstructionCommand>(value).is_err());
    }
}
