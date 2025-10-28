use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type ClientId = Uuid;

pub type CellKey = (i32, i32);

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct IVec2 { pub x:i32, pub y:i32 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TownHall {
    pub id: String,
    pub cell: CellKey,
    pub origin: IVec2, // anchor tile (top-left once you move to 3x3)
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTownResponse {
    pub ok: bool,
    pub error: Option<String>,
    pub town: Option<Town>,
    pub spawn_tile: Option<IVec2>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnPlayerAt {
    pub player_id: ClientId,
    pub tile: IVec2,
}

// subjects
pub const SUBJ_TOWN_CREATE: &str = "twz.town.create";
pub fn subj_cell_spawn(cell:&CellKey) -> String {
    format!("twz.cell.{}.{}.spawn", cell.0, cell.1)
}

// spawn logic: 1 right, 3 down from hall origin
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
    pub http_addr: String, // optional metadata
    pub ws_addr: String,   // optional metadata
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

/* ---------- NATS handoff messages ---------- */

#[derive(Debug, Serialize, Deserialize, Clone)]
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
    pub team: u8,
    // input hints if you want: last_input, etc
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