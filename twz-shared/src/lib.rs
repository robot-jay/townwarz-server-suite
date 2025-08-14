use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type ClientId = Uuid;

pub type CellKey = (i32, i32);

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