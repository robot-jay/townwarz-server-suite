use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod domain;
pub use domain::*;

pub type ClientId = Uuid;

pub type CellKey = (i32, i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IVec2 { pub x:i32, pub y:i32 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TownHall {
    pub id: String,
    pub cell: CellKey,
    pub origin: IVec2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Town {
    pub id: String,
    pub name: String,
    pub hall: TownHall,
    pub created_at_ms: i64,
    pub mayor_player_id: ClientId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTownRequest {
    pub player_id: ClientId,
    pub desired_name: String,
    pub user: GameUser,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameUser {
    pub sub: String,
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GameEntry {
    StartGame { id_token: String, role: String, town_name: String },
    ResumeGame { id_token: String },
    QuitGame { id_token: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GameSession { pub player_id: ClientId }

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackendHello {
    SetPid {
        id: ClientId,
        #[serde(default)] town: Option<Town>,
        #[serde(default)] identity: Option<GameIdentity>,
        #[serde(default)] resume_state: Option<PlayerState>,
    },
}

pub const TILE_PX: f32 = 48.0;
pub const CELL_TILES: i32 = 64;
pub const TOWN_GRID: i32 = 32;
pub const TOWN_OFFSET: i32 = TOWN_GRID / 2;

pub fn validate_town_name(name: &str) -> Result<String, &'static str> {
    let name = name.trim();
    if !(2..=32).contains(&name.chars().count()) {
        return Err("Town names must be between 2 and 32 characters.");
    }
    if !name.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '\'')) {
        return Err("Use letters, numbers, spaces, apostrophes, or hyphens in your town name.");
    }
    Ok(name.to_owned())
}

// Expand a square around the origin: (0,0), (0,1), (1,1), (1,0), ...
// These are global tile coordinates, identical to the procedural terrain grid.
pub fn hall_for_slot(slot: i32) -> TownHall {
    assert!(slot >= 0);
    let edge = (slot as f64).sqrt().floor() as i32;
    let offset = slot - edge * edge;
    let (x, y) = if offset <= edge { (offset, edge) } else { (edge, 2 * edge - offset) };
    let origin = IVec2 { x: TOWN_OFFSET + x * TOWN_GRID, y: TOWN_OFFSET + y * TOWN_GRID };
    TownHall {
        id: format!("h-{}-{}", origin.x, origin.y),
        cell: (origin.x.div_euclid(CELL_TILES), origin.y.div_euclid(CELL_TILES)),
        origin,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTownResponse {
    pub ok: bool,
    pub error: Option<String>,
    pub town: Option<Town>,
    pub spawn_tile: Option<IVec2>,
    #[serde(default)] pub connection_id: Option<ClientId>,
    #[serde(default)] pub player_state: Option<PlayerState>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResumeGameRequest { pub user: GameUser }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameConnection {
    pub player_id: ClientId,
    pub connection_id: ClientId,
    #[serde(default)] pub checkpoint: Option<PlayerState>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameEndReason { #[default] IntentionalQuit, Timeout, Orbed, SetupFailed }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndGameRequest {
    pub player_id: ClientId,
    #[serde(default)] pub connection_id: Option<ClientId>,
    #[serde(default)] pub reason: GameEndReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameEnded { pub player_id: ClientId, pub reason: GameEndReason }

pub const RECONNECT_GRACE_SECONDS: u64 = 15 * 60;
pub const SUBJ_GAME_RESUME: &str = "twz.game.resume";
pub const SUBJ_GAME_DISCONNECT: &str = "twz.game.disconnect";
pub const SUBJ_GAME_QUIT_USER: &str = "twz.game.quit_user";
pub const SUBJ_GAME_ENDED: &str = "twz.game.ended";
pub const SUBJ_PROMOTION_REQUESTED: &str = "twz.town.promotion.requested";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnPlayerAt {
    pub player_id: ClientId,
    pub tile: IVec2,
}

// subjects
pub const SUBJ_TOWN_CREATE: &str = "twz.town.create";
pub const SUBJ_TOWN_LIST: &str = "twz.town.list";
pub const SUBJ_GAME_END: &str = "twz.game.end";
pub const SUBJ_GAME_TOUCH: &str = "twz.game.touch";
pub const SUBJ_CELL_SPAWN_ALL: &str = "twz.cell.*.*.spawn";
pub fn subj_cell_spawn(cell:&CellKey) -> String {
    format!("twz.cell.{}.{}.spawn", cell.0, cell.1)
}

pub const SPAWN_OFFSET: IVec2 = IVec2 { x: 1, y: 3 };
#[inline]
pub fn spawn_from_hall(h: IVec2) -> IVec2 { IVec2{ x: h.x + SPAWN_OFFSET.x, y: h.y + SPAWN_OFFSET.y } }

#[inline]
pub fn cell_of(x: f32, y: f32, w: f32) -> CellKey {
    let cx = (x / w).floor() as i32;
    let cy = (y / w).floor() as i32;
    (cx, cy)
}

#[inline]
pub fn neighbor_cells(c: CellKey) -> [CellKey; 9] {
    let (cx, cy) = c;
    [
        (cx - 1, cy - 1),
        (cx, cy - 1),
        (cx + 1, cy - 1),
        (cx - 1, cy),
        (cx, cy),
        (cx + 1, cy),
        (cx - 1, cy + 1),
        (cx, cy + 1),
        (cx + 1, cy + 1),
    ]
}

/* ---------- Conductor API ---------- */

#[derive(Debug, Serialize, Deserialize)]
pub struct ServerRegister {
    pub server_id: String,
    pub http_addr: String, 
    pub ws_addr: String, 
    pub capacity_hint: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Heartbeat {
    pub server_id: String,
    pub tick_ms_p50: f32,
    pub tick_ms_p95: f32,
    pub players: usize,
    pub bullets: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RouteCellResponse {
    pub owner_server_id: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct PlayerState {
    pub id: ClientId,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub ang: f32,
    pub hp: i32,
    pub max_hp: i32,
    pub respawn_at: Option<u64>,
    pub game: PlayerGameState,
    pub damage_contributions: Vec<DamageContribution>,
    #[serde(default)]
    pub spawn_tile: Option<IVec2>,
}

impl PlayerState {
    pub fn initial_for_town(town:&Town) -> Self {
        let tile=spawn_from_hall(town.hall.origin);
        Self { id:town.mayor_player_id, x:tile.x as f32*TILE_PX, y:tile.y as f32*TILE_PX,
            vx:0.0,vy:0.0,ang:0.0,hp:100,max_hp:100,respawn_at:None,
            game:PlayerGameState::new(GameIdentity::mayor(town.mayor_player_id,town.id.clone())),
            damage_contributions:Vec::new(),spawn_tile:Some(tile) }
    }
}

#[cfg(test)]
mod town_tests {
    use super::*;

    #[test]
    fn town_names_are_trimmed_and_validated() {
        assert_eq!(validate_town_name("  New Hope  ").unwrap(), "New Hope");
        for name in ["", " ", "A", "<script>", "Town\nName"] {
            assert!(validate_town_name(name).is_err());
        }
        assert!(validate_town_name(&"a".repeat(33)).is_err());
        assert!(validate_town_name("Élan's Crossing-2").is_ok());
    }

    #[test]
    fn allocated_halls_are_unique_and_match_terrain_and_routing() {
        let mut seen = std::collections::HashSet::new();
        for slot in 0..1000 {
            let hall = hall_for_slot(slot);
            assert!(seen.insert((hall.origin.x, hall.origin.y)));
            assert_eq!((hall.origin.x - TOWN_OFFSET).rem_euclid(TOWN_GRID), 0);
            assert_eq!((hall.origin.y - TOWN_OFFSET).rem_euclid(TOWN_GRID), 0);
            let spawn = spawn_from_hall(hall.origin);
            assert_eq!(cell_of(spawn.x as f32 * TILE_PX, spawn.y as f32 * TILE_PX, CELL_TILES as f32 * TILE_PX), hall.cell);
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HandoffRequest {
    pub from_server: String,
    pub to_server: String,
    pub player: PlayerState,
    pub ts_ms: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HandoffAck {
    pub to_server: String,
    pub from_server: String,
    pub player_id: ClientId,
    pub accepted: bool,
    pub ts_ms: u64,
}
