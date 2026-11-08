use twz_shared::{
    cell_of,
    BackendHello,
    BuildingInstance,
    CellKey,
    ClientId,
    Combatant,
    DamageAttribution,
    DamageContribution,
    DamageSourceKind,
    DefeatRecord,
    GameEnded,
    GameIdentity,
    HandoffAck,
    HandoffRequest,
    Heartbeat,
    //spawn + town glue
    IVec2,
    PlayerAffiliation,
    PlayerGameState,
    PlayerState,
    RewardTargetKind,
    RouteCellResponse,
    ServerRegister,
    Town,
    WorldRect,
    CELL_TILES,
    DOMAIN_VERSION,
    SUBJ_CELL_SPAWN_ALL,
    SUBJ_GAME_ENDED,
    SUBJ_TOWN_LIST,
    TILE_PX,
    TOWN_GRID,
    TOWN_OFFSET,
};

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::{
    net::TcpListener,
    sync::{mpsc, RwLock},
};
use uuid::Uuid;

mod buildings;
mod economy;
use buildings::BuildingStore;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    id: String,

    #[arg(long, default_value = "0.0.0.0:3001")]
    bind: String,

    #[arg(long)]
    advertise: Option<String>,

    #[arg(long, default_value = "http://127.0.0.1:7000")]
    conductor: String,

    #[arg(long, default_value = "127.0.0.1:4222")]
    nats: String,
}

#[derive(Clone)]
struct AppState {
    server_id: String,
    conductor: String,
    nats: async_nats::Client,
    clients: Arc<RwLock<HashMap<ClientId, mpsc::UnboundedSender<Message>>>>,
    world: Arc<RwLock<World>>,
    input_tx: mpsc::UnboundedSender<(ClientId, InputMsg)>,
    ctrl_tx: mpsc::UnboundedSender<ControlMsg>,
}

#[derive(serde::Serialize, Clone)]
#[serde(tag = "type")]
enum ServerEvent {
    Hit {
        src: ClientId,
        dst: ClientId,
        dmg: i32,
        hp: i32,
    },
    Death {
        id: ClientId,
    },
    Respawn {
        id: ClientId,
        x: f32,
        y: f32,
    },
    Defeat {
        record: DefeatRecord,
    },
}

#[derive(Clone, Serialize)]
struct Player {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    ang: f32,
    #[serde(skip)]
    hp: i32,
    #[serde(skip)]
    last_cell: CellKey,
    #[serde(skip)]
    transferring: bool,
    #[serde(skip)]
    economy_pending: bool,
    max_hp: i32,
    respawn_at: Option<u64>,
    game: PlayerGameState,
    #[serde(skip)]
    damage_contributions: Vec<DamageContribution>,
    #[serde(skip)]
    transfer_target: Option<String>,
    #[serde(skip)]
    transfer_started_at_ms: Option<u64>,
    #[serde(skip)]
    spawn_tile: Option<IVec2>,
}

type BulletId = u64;

#[derive(Clone, Serialize)]
struct Bullet {
    id: BulletId,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    owner: ClientId,
    born_ms: u64,
    credit: DamageAttribution,
    #[serde(skip)]
    previous_x: f32,
    #[serde(skip)]
    previous_y: f32,
}

impl Player {
    fn new(id: ClientId, identity: GameIdentity, spawn_tile: Option<IVec2>) -> Self {
        let (x, y) = player_spawn_point(id, spawn_tile);
        Self {
            x,
            y,
            vx: 0.0,
            vy: 0.0,
            ang: 0.0,
            hp: 100,
            max_hp: 100,
            respawn_at: None,
            last_cell: cell_of(x, y, CELL_W),
            transferring: false,
            economy_pending: false,
            transfer_target: None,
            transfer_started_at_ms: None,
            spawn_tile,
            game: PlayerGameState::new(identity),
            damage_contributions: Vec::new(),
        }
    }

    fn handoff_state(&self, id: ClientId) -> PlayerState {
        PlayerState {
            id,
            x: self.x,
            y: self.y,
            vx: self.vx,
            vy: self.vy,
            ang: self.ang,
            hp: self.hp,
            max_hp: self.max_hp,
            respawn_at: self.respawn_at,
            game: self.game.clone(),
            damage_contributions: self.damage_contributions.clone(),
            spawn_tile: self.spawn_tile,
        }
    }

    fn from_handoff(player: PlayerState) -> Self {
        Self {
            x: player.x,
            y: player.y,
            vx: player.vx,
            vy: player.vy,
            ang: player.ang,
            hp: player.hp,
            max_hp: player.max_hp,
            respawn_at: player.respawn_at,
            last_cell: cell_of(player.x, player.y, CELL_W),
            transferring: false,
            economy_pending: false,
            transfer_target: None,
            transfer_started_at_ms: None,
            game: player.game,
            damage_contributions: player.damage_contributions,
            spawn_tile: player.spawn_tile,
        }
    }

    fn take_damage(
        &mut self,
        id: ClientId,
        damage: i32,
        credit: &DamageAttribution,
        now_ms: u64,
    ) -> Vec<ServerEvent> {
        if damage <= 0
            || self.hp <= 0
            || self.transferring
            || self.economy_pending
            || !credit.can_damage(&self.game.identity.combatant(id))
        {
            return Vec::new();
        }
        let actual_damage = damage.min(self.hp) as u32;
        self.hp = self.hp.saturating_sub(damage).max(0);
        if let Some(contribution) = self
            .damage_contributions
            .iter_mut()
            .find(|entry| entry.credit == *credit)
        {
            contribution.damage = contribution.damage.saturating_add(actual_damage);
            contribution.last_hit_at_ms = now_ms;
        } else {
            self.damage_contributions.push(DamageContribution {
                credit: credit.clone(),
                damage: actual_damage,
                last_hit_at_ms: now_ms,
            });
        }
        let src = credit
            .beneficiary
            .as_ref()
            .map(|owner| owner.player_id)
            .unwrap_or(credit.source_entity_id);
        let mut events = vec![ServerEvent::Hit {
            src,
            dst: id,
            dmg: actual_damage as i32,
            hp: self.hp,
        }];
        if self.hp == 0 {
            self.respawn_at = Some(now_ms + 3000);
            self.game.on_combat_death();
            events.push(ServerEvent::Death { id });
            events.push(ServerEvent::Defeat {
                record: DefeatRecord {
                    event_id: Uuid::new_v4(),
                    target_id: id,
                    target_game_id: self.game.identity.game_id,
                    target_kind: RewardTargetKind::Player,
                    credit: credit.clone(),
                    contributions: std::mem::take(&mut self.damage_contributions),
                    occurred_at_ms: now_ms,
                },
            });
        }
        events
    }
}

#[derive(Default, Clone, Serialize)]
struct World {
    towns: Vec<Town>,
    players: HashMap<ClientId, Player>,
    bullets: HashMap<BulletId, Bullet>,
    #[serde(skip)]
    last_input: HashMap<ClientId, InputState>,
    #[serde(skip)]
    last_shoot: HashMap<ClientId, bool>,
    #[serde(skip)]
    next_bullet_id: BulletId,
    #[serde(skip)]
    grid_players: HashMap<CellKey, HashSet<ClientId>>,
    #[serde(skip)]
    grid_bullets: HashMap<CellKey, HashSet<BulletId>>,
    terrain_chunks: HashMap<(i32, i32), Vec<u8>>, //each chunk is CHUNK_TILES*CHUNK_TILES bytes
    #[serde(skip)]
    buildings: BuildingStore,
    #[serde(skip)]
    ended_games: HashSet<ClientId>,
    #[serde(skip)]
    last_town_change: Option<Instant>,
    #[serde(skip)]
    pending_construction: HashMap<ClientId, economy::PendingConstruction>,
    #[serde(skip)]
    durable_buildings: HashMap<ClientId, HashSet<Uuid>>,
    #[serde(skip)]
    economy_revisions: HashMap<ClientId, u64>,
}

#[derive(Debug, Clone, Default)]
struct InputState {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    shoot: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMsg {
    Input {
        up: bool,
        down: bool,
        left: bool,
        right: bool,
        shoot: bool,
        ts: u64,
    },
    Ping,
    PauseGame,
}

#[derive(Debug, Clone)]
struct InputMsg {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    shoot: bool,
    ts: u64,
}

#[derive(Debug)]
enum ControlMsg {
    PrepareConstruction {
        preparation: twz_shared::ConstructionPreparation,
        reply: tokio::sync::oneshot::Sender<Option<twz_shared::ConstructionError>>,
    },
    ApplyEconomy {
        projection: twz_shared::EconomyProjection,
        requested_at: Option<Instant>,
    },
    Spawn {
        id: ClientId,
        town: Option<Town>,
        identity: GameIdentity,
        resume_state: Option<PlayerState>,
    },
    Despawn(ClientId),
    ControlSpawnAt {
        player_id: ClientId,
        tile: IVec2,
    },
    AdoptHandoff(HandoffRequest),
    AcknowledgeHandoff(HandoffAck),
    SyncTowns {
        towns: Vec<Town>,
        requested_at: Instant,
    },
    EndGame(GameEnded),
    PauseGame(ClientId),
    InsertBuilding {
        building: BuildingInstance,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    RemoveBuilding {
        id: Uuid,
        game_id: Uuid,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMsg<'a> {
    Hello {
        id: ClientId,
    },
    State {
        t: u64,
        me: ClientId,
        players: Vec<(ClientId, f32, f32, f32, i32)>,
        bullets: Vec<&'a Bullet>,
    },
    Pong,
}

const CELL_W: f32 = CELL_TILES as f32 * TILE_PX;
const CHUNK_TILES: i32 = 64; //64×64 tiles per cached chunk (tweakable)
const TOWN_CLEAR_RAD: i32 = 4; //tiles of guaranteed ground around a town hall
const TERRAIN_SEED: u64 = 0xC0FFEE_BABE_5EED; //change to remix the world

const TILE: f32 = TILE_PX; //48.0 px tiles (you already have TILE_PX)
const PLAYER_RADIUS: f32 = 16.0; //tweak to taste
const MAX_PENETRATION_ITER: usize = 3;

impl Tile {
    #[inline]
    fn blocks_player(self) -> bool {
        matches!(self, Tile::Water | Tile::Rock)
    }
    #[inline]
    fn blocks_bullet(self) -> bool {
        matches!(self, Tile::Rock)
    }
}

#[inline]
fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

fn resolve_circle_vs_tiles(world: &mut World, x: &mut f32, y: &mut f32, r: f32) {
    //check a small neighborhood of tiles; iterate a few times in case of multiple overlaps
    for _ in 0..MAX_PENETRATION_ITER {
        let mut pushed = false;

        let min_tx = ((*x - r) / TILE).floor() as i32;
        let max_tx = (((*x + r) / TILE).floor() as i32) + 1;
        let min_ty = ((*y - r) / TILE).floor() as i32;
        let max_ty = (((*y + r) / TILE).floor() as i32) + 1;

        'tileloop: for ty in min_ty..=max_ty {
            for tx in min_tx..=max_tx {
                let t = tile_at(world, tx, ty);
                if !t.blocks_player() {
                    continue;
                }

                let rx0 = tx as f32 * TILE;
                let ry0 = ty as f32 * TILE;
                let rx1 = rx0 + TILE;
                let ry1 = ry0 + TILE;

                let cx = clamp(*x, rx0, rx1);
                let cy = clamp(*y, ry0, ry1);

                let dx = *x - cx;
                let dy = *y - cy;
                let d2 = dx * dx + dy * dy;

                if d2 < r * r {
                    let d = d2.sqrt().max(0.0001);
                    let nx = dx / d;
                    let ny = dy / d;
                    let pen = r - d;
                    *x += nx * pen;
                    *y += ny * pen;
                    pushed = true;
                    break 'tileloop;
                }
            }
        }
        if !pushed {
            break;
        }
    }
}

fn resolve_vs_enemy_players(
    me_id: ClientId,
    me: &Combatant,
    x: &mut f32,
    y: &mut f32,
    world_players: &std::collections::HashMap<ClientId, Player>,
) {
    for (pid, other) in world_players.iter() {
        if *pid == me_id {
            continue;
        }
        if me.is_friendly_to(&other.game.identity.combatant(*pid)) {
            continue;
        }
        if other.hp <= 0 {
            continue;
        } //ignore dead bodies

        let dx = *x - other.x;
        let dy = *y - other.y;
        let d2 = dx * dx + dy * dy;
        let min_dist = 2.0 * PLAYER_RADIUS;
        if d2 < min_dist * min_dist {
            let d = d2.sqrt().max(0.0001);
            let nx = dx / d;
            let ny = dy / d;
            let pen = min_dist - d;
            *x += nx * pen;
            *y += ny * pen;
        }
    }
}

//Tile codes
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tile {
    Ground = 0,
    Water = 1,
    Rock = 2,
}
impl Tile {
    #[inline]
    fn is_solid(self) -> bool {
        matches!(self, Tile::Water | Tile::Rock)
    }
}

//World coordinate <-> tile/chunk helpers
#[inline]
fn world_to_tile(x: f32, y: f32) -> (i32, i32) {
    let tx = (x / TILE_PX).floor() as i32;
    let ty = (y / TILE_PX).floor() as i32;
    (tx, ty)
}
#[inline]
fn tile_to_chunk(tx: i32, ty: i32) -> (i32, i32) {
    let cx = div_floor(tx, CHUNK_TILES);
    let cy = div_floor(ty, CHUNK_TILES);
    (cx, cy)
}
#[inline]
fn div_floor(a: i32, b: i32) -> i32 {
    let d = a / b;
    let r = a % b;
    if (r != 0) && ((r < 0) != (b < 0)) {
        d - 1
    } else {
        d
    }
}
#[inline]
fn pos_in_chunk(tx: i32, ty: i32) -> (i32, i32) {
    let (cx, cy) = tile_to_chunk(tx, ty);
    (tx - cx * CHUNK_TILES, ty - cy * CHUNK_TILES)
}

//simple 2D hash -> [0,1)
#[inline]
fn hash2(x: i32, y: i32, seed: u64) -> f32 {
    let mut z = (x as u64).wrapping_mul(0x9E3779B97F4A7C15)
        ^ (y as u64).wrapping_mul(0xC2B2AE3D27D4EB4F)
        ^ seed;
    //splitmix64
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    let v = z ^ (z >> 31);
    //map lower 24 bits to [0,1)
    ((v & 0xFFFFFF) as f32) / 16_777_216.0
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
fn smoothstep(t: f32) -> f32 {
    //smoother interpolation
    t * t * (3.0 - 2.0 * t)
}

//value noise with bilinear smoothing at integer lattice
fn value_noise(x: f32, y: f32, seed: u64) -> f32 {
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = x - x.floor();
    let fy = y - y.floor();

    let v00 = hash2(x0, y0, seed);
    let v10 = hash2(x0 + 1, y0, seed);
    let v01 = hash2(x0, y0 + 1, seed);
    let v11 = hash2(x0 + 1, y0 + 1, seed);

    let sx = smoothstep(fx);
    let sy = smoothstep(fy);

    let ix0 = lerp(v00, v10, sx);
    let ix1 = lerp(v01, v11, sx);
    lerp(ix0, ix1, sy)
}

//basic fBM (fractal brownian motion) for blob shapes
fn fbm(mut x: f32, mut y: f32, seed: u64) -> f32 {
    let mut amp = 1.0;
    let mut freq = 1.0 / 12.0; //base period ~12 tiles
    let mut sum = 0.0;
    let mut norm = 0.0;
    for _ in 0..4 {
        sum += value_noise(x * freq, y * freq, seed) * amp;
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / norm
}

#[inline]
fn near_town_clear(tx: i32, ty: i32) -> bool {
    let rx = (tx - TOWN_OFFSET).rem_euclid(TOWN_GRID);
    let ry = (ty - TOWN_OFFSET).rem_euclid(TOWN_GRID);
    let dx = rx.min(TOWN_GRID - rx);
    let dy = ry.min(TOWN_GRID - ry);
    dx <= TOWN_CLEAR_RAD && dy <= TOWN_CLEAR_RAD
}

fn terrain_at_tile(tx: i32, ty: i32) -> Tile {
    if near_town_clear(tx, ty) {
        return Tile::Ground;
    }

    let w_n = fbm(tx as f32, ty as f32, TERRAIN_SEED ^ 0xA5A5A5A5);
    let r_n = fbm(tx as f32, ty as f32, TERRAIN_SEED ^ 0x5A5A5A5A);

    if w_n < 0.35 {
        return Tile::Water;
    }
    if r_n > 0.68 {
        return Tile::Rock;
    }
    Tile::Ground
}
#[derive(serde::Serialize)]
struct TileWindow {
    ox: i32,
    oy: i32,
    w: i32,
    h: i32,
    data: Vec<u8>,
}

fn collect_tile_window(world: &mut World, ox: i32, oy: i32, w: i32, h: i32) -> TileWindow {
    let mut data = Vec::with_capacity((w * h) as usize);
    for ty in oy..(oy + h) {
        for tx in ox..(ox + w) {
            data.push(tile_at(world, tx, ty) as u8);
        }
    }
    TileWindow { ox, oy, w, h, data }
}

fn generate_chunk(cx: i32, cy: i32) -> Vec<u8> {
    let mut v = vec![0u8; (CHUNK_TILES * CHUNK_TILES) as usize];
    let base_tx = cx * CHUNK_TILES;
    let base_ty = cy * CHUNK_TILES;
    for j in 0..CHUNK_TILES {
        for i in 0..CHUNK_TILES {
            let tx = base_tx + i;
            let ty = base_ty + j;
            v[(j * CHUNK_TILES + i) as usize] = terrain_at_tile(tx, ty) as u8;
        }
    }
    v
}

fn get_chunk<'a>(world: &'a mut World, cx: i32, cy: i32) -> &'a Vec<u8> {
    world.buildings.ensure_chunk(cx, cy);
    world
        .terrain_chunks
        .entry((cx, cy))
        .or_insert_with(|| generate_chunk(cx, cy))
}

fn tile_at(world: &mut World, tx: i32, ty: i32) -> Tile {
    let (cx, cy) = tile_to_chunk(tx, ty);
    let (ix, iy) = pos_in_chunk(tx, ty);
    let chunk = get_chunk(world, cx, cy);
    let idx = (iy * CHUNK_TILES + ix) as usize;
    match chunk[idx] {
        0 => Tile::Ground,
        1 => Tile::Water,
        2 => Tile::Rock,
        _ => Tile::Ground,
    }
}

fn tile_at_world(world: &mut World, x: f32, y: f32) -> Tile {
    let (tx, ty) = world_to_tile(x, y);
    tile_at(world, tx, ty)
}

fn container_hostname() -> Option<String> {
    match std::fs::read_to_string("/etc/hostname") {
        Ok(s) => {
            let h = s.trim();
            if h.is_empty() {
                None
            } else {
                Some(h.to_string())
            }
        }
        Err(_) => None,
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("info".parse().unwrap()),
        )
        .with_target(false)
        .init();

    let nats = async_nats::connect(args.nats.clone()).await?;
    let nats_sub_req = nats.subscribe(format!("handoff.{}.req", args.id)).await?;
    let nats_sub_ack = nats.subscribe(format!("handoff.{}.ack", args.id)).await?;

    let (input_tx, input_rx) = mpsc::unbounded_channel::<(ClientId, InputMsg)>();
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel::<ControlMsg>();
    let state = AppState {
        server_id: args.id.clone(),
        conductor: args.conductor.clone(),
        nats: nats.clone(),
        clients: Arc::new(RwLock::new(HashMap::new())),
        world: Arc::new(RwLock::new(World::default())),
        input_tx,
        ctrl_tx,
    };

    // All cells see active town names, including halls belonging to other cells.
    // Refresh after reconnect/startup as well as creation and lease expiry.
    tokio::spawn(town_refresh_task(state.clone()));
    tokio::spawn(spawn_command_task(state.clone()));
    tokio::spawn(game_end_task(state.clone()));
    economy::serve(state.clone()).await?;

    {
        let mut w = state.world.write().await;
        let c = get_chunk(&mut w, 0, 0);
        println!("chunk (0,0) first 32 tiles: {:?}", &c[0..32.min(c.len())]);
    }

    //register
    let advertise_addr = args
        .advertise
        .clone()
        .or_else(|| {
            if let Some((host, port)) = args.bind.rsplit_once(':') {
                if host == "0.0.0.0" {
                    if let Some(hn) = container_hostname() {
                        return Some(format!("{}:{}", hn, port));
                    }
                }
            }
            None
        })
        .unwrap_or_else(|| args.bind.clone());
    let http_client = reqwest::Client::new();
    let registration = ServerRegister {
        server_id: args.id.clone(),
        http_addr: format!("http://{}", advertise_addr),
        ws_addr: format!("ws://{}", advertise_addr),
        capacity_hint: 1000,
    };
    http_client
        .post(format!("{}/register", &args.conductor))
        .json(&registration)
        .send()
        .await?
        .error_for_status()?;

    tokio::spawn(heartbeat_task(
        state.clone(),
        http_client.clone(),
        args.conductor.clone(),
        registration,
    ));

    tokio::spawn(game_loop(
        state.clone(),
        input_rx,
        ctrl_rx,
        http_client.clone(),
    ));

    tokio::spawn(handoff_req_consumer(state.clone(), nats_sub_req));
    tokio::spawn(handoff_ack_consumer(state.clone(), nats_sub_ack));

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(|| async { "ok" }))
        .with_state(state);

    let addr: SocketAddr = args.bind.parse().unwrap();
    println!("[{}] listening on {}", args.id, addr);
    axum::serve(TcpListener::bind(addr).await?, app).await?;
    Ok(())
}

// Anonymous debug connections still use the shared default hall.
fn spawn_point_for(_id: ClientId) -> (f32, f32) {
    let hall = IVec2 {
        x: TOWN_OFFSET,
        y: TOWN_OFFSET,
    };
    let tile = twz_shared::spawn_from_hall(hall);
    (tile.x as f32 * TILE_PX, tile.y as f32 * TILE_PX)
}

impl World {
    fn attach_player(&mut self, id: ClientId, town: Option<Town>, identity: GameIdentity) -> bool {
        // A backend reconnect after handoff attaches to the transferred state;
        // it must never reinitialize the wallet, inventory or cooldowns.
        let spawn_tile = town
            .as_ref()
            .map(|town| twz_shared::spawn_from_hall(town.hall.origin));
        if let Some(town) = town {
            if !self.buildings.claim(&town) {
                return false;
            }
            self.last_town_change = Some(Instant::now());
            self.towns.retain(|existing| existing.id != town.id);
            self.towns.push(town);
        }
        self.players
            .entry(id)
            .or_insert_with(|| Player::new(id, identity, spawn_tile));
        self.last_input.entry(id).or_default();
        self.last_shoot.entry(id).or_insert(false);
        true
    }

    fn adopt_handoff(&mut self, player: PlayerState) -> bool {
        if self.ended_games.contains(&player.game.identity.game_id) {
            return false;
        }
        let id = player.id;
        if let Some(existing) = self.players.get(&id) {
            // Retransmission may acknowledge an already adopted player, but
            // must not overwrite newer state or attach a different game.
            return existing.game.identity == player.game.identity;
        }
        self.players.insert(id, Player::from_handoff(player));
        self.last_input.entry(id).or_default();
        self.last_shoot.entry(id).or_insert(false);
        true
    }

    fn remove_player(&mut self, id: ClientId) {
        self.players.remove(&id);
        self.last_input.remove(&id);
        self.last_shoot.remove(&id);
    }

    fn complete_handoff(&mut self, ack: &HandoffAck) -> bool {
        let matches_pending = self.players.get(&ack.player_id).is_some_and(|player| {
            player.transferring && player.transfer_target.as_deref() == Some(ack.to_server.as_str())
        });
        if !matches_pending {
            return false;
        }
        if ack.accepted {
            self.remove_player(ack.player_id);
            true
        } else {
            let player = self.players.get_mut(&ack.player_id).unwrap();
            player.transferring = false;
            player.transfer_target = None;
            player.transfer_started_at_ms = None;
            false
        }
    }

    fn sync_towns(&mut self, towns: Vec<Town>, requested_at: Instant) {
        if self
            .last_town_change
            .is_some_and(|changed| requested_at < changed)
        {
            return;
        }
        let towns: Vec<_> = towns
            .into_iter()
            .filter(|town| !self.ended_games.contains(&town.mayor_player_id))
            .collect();
        let active: HashSet<_> = towns.iter().map(|town| town.mayor_player_id).collect();
        let expired: Vec<_> = self
            .towns
            .iter()
            .filter(|town| !active.contains(&town.mayor_player_id))
            .map(|town| town.mayor_player_id)
            .collect();
        for id in expired {
            self.end_game(id);
        }
        self.buildings.reconcile(&towns);
        self.towns = towns;
    }

    fn end_game(&mut self, id: ClientId) {
        self.pending_construction.remove(&id);
        self.durable_buildings.remove(&id);
        self.economy_revisions.remove(&id);
        self.ended_games.insert(id);
        self.last_town_change = Some(Instant::now());
        self.buildings.remove_game(id);
        self.towns.retain(|town| town.mayor_player_id != id);
        self.bullets.retain(|_, bullet| {
            !bullet
                .credit
                .beneficiary
                .as_ref()
                .is_some_and(|owner| owner.game_id == id)
        });
        let players: Vec<_> = self
            .players
            .iter()
            .filter(|(_, player)| player.game.identity.game_id == id)
            .map(|(id, _)| *id)
            .collect();
        for player in players {
            self.remove_player(player);
        }
    }

    fn insert_building(&mut self, mut building: BuildingInstance) -> Result<(), &'static str> {
        let owner = building
            .ownership
            .as_ref()
            .ok_or("constructed buildings require ownership")?;
        if self.ended_games.contains(&owner.game_id)
            || !self.towns.iter().any(|town| {
                town.mayor_player_id == owner.game_id
                    && Some(town.id.as_str()) == owner.town_id.as_deref()
            })
        {
            return Err("building owner has no active town");
        }
        let definition = building.building_type.definition();
        if definition.status != twz_shared::DefinitionStatus::Specified {
            return Err("building definition is unavailable");
        }
        if building.building_type == twz_shared::BuildingType::TownHall {
            return Err("townhalls are managed by the town lifecycle");
        }
        building.health = twz_shared::Health {
            current: definition.max_health,
            maximum: definition.max_health,
        };
        let geometry = building.building_type.definition().geometry;
        for y in 0..geometry.footprint_tiles.y {
            for x in 0..geometry.footprint_tiles.x {
                let tile = (building.origin_tile.x + x, building.origin_tile.y + y);
                self.buildings
                    .ensure_chunk(tile.0.div_euclid(CELL_TILES), tile.1.div_euclid(CELL_TILES));
            }
        }
        self.buildings.insert(building, |x, y| {
            matches!(terrain_at_tile(x, y), Tile::Ground)
        })
    }

    fn remove_building(&mut self, id: Uuid, game_id: Uuid) -> Result<(), &'static str> {
        let building = self
            .buildings
            .buildings
            .get(&id)
            .ok_or("building not found")?;
        if building.building_type == twz_shared::BuildingType::TownHall {
            return Err("townhalls are managed by the town lifecycle");
        }
        if !building
            .ownership
            .as_ref()
            .is_some_and(|owner| owner.game_id == game_id)
        {
            return Err("building belongs to another game");
        }
        self.buildings.remove(id);
        Ok(())
    }
}

fn player_spawn_point(id: ClientId, tile: Option<IVec2>) -> (f32, f32) {
    tile.map(|tile| (tile.x as f32 * TILE_PX, tile.y as f32 * TILE_PX))
        .unwrap_or_else(|| spawn_point_for(id))
}

async fn town_refresh_task(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        interval.tick().await;
        let requested_at = Instant::now();
        if let Ok(Ok(message)) = tokio::time::timeout(
            Duration::from_secs(2),
            state.nats.request(SUBJ_TOWN_LIST, "".into()),
        )
        .await
        {
            if let Ok(towns) = serde_json::from_slice::<Vec<Town>>(&message.payload) {
                let _ = state.ctrl_tx.send(ControlMsg::SyncTowns {
                    towns,
                    requested_at,
                });
            }
        }
    }
}

async fn game_end_task(state: AppState) {
    let Ok(mut sub) = state.nats.subscribe(SUBJ_GAME_ENDED).await else {
        return;
    };
    while let Some(message) = sub.next().await {
        if let Ok(event) = serde_json::from_slice::<GameEnded>(&message.payload) {
            let _ = state.ctrl_tx.send(ControlMsg::EndGame(event));
        }
    }
}

// Keep the cross-service spawn contract functional for already attached players.
// New-game initial placement travels with set_pid, avoiding a publish-before-
// connection race; later explicit spawn commands use the owning cell only.
async fn spawn_command_task(state: AppState) {
    let Ok(mut sub) = state.nats.subscribe(SUBJ_CELL_SPAWN_ALL).await else {
        return;
    };
    let route = RouteClient {
        http: reqwest::Client::new(),
        base: state.conductor.clone(),
    };
    while let Some(message) = sub.next().await {
        if let Ok(command) = serde_json::from_slice::<twz_shared::SpawnPlayerAt>(&message.payload) {
            let cell = (
                command.tile.x.div_euclid(CELL_TILES),
                command.tile.y.div_euclid(CELL_TILES),
            );
            if message.subject.as_str() != twz_shared::subj_cell_spawn(&cell) {
                continue;
            }
            if route.owner_of(cell.0, cell.1).await.ok().as_deref()
                == Some(state.server_id.as_str())
            {
                let _ = state.ctrl_tx.send(ControlMsg::ControlSpawnAt {
                    player_id: command.player_id,
                    tile: command.tile,
                });
            }
        }
    }
}

async fn ws_handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| client_socket(state, socket))
}

async fn client_socket(state: AppState, socket: WebSocket) {
    let (ws_sink, mut ws_stream) = socket.split();

    let mut forced_id: Option<Uuid> = None;
    let mut initial_town: Option<Town> = None;
    let mut initial_identity: Option<GameIdentity> = None;
    let mut resume_state: Option<PlayerState> = None;
    let mut prebuffer: Vec<Message> = Vec::new();

    if let Some(Ok(msg)) = ws_stream.next().await {
        match msg {
            Message::Text(txt) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                    if v.get("type").and_then(|t| t.as_str()) == Some("set_pid") {
                        if let Some(s) = v.get("id").and_then(|idv| idv.as_str()) {
                            if let Ok(parsed) = Uuid::parse_str(s) {
                                forced_id = Some(parsed);
                                if let Ok(BackendHello::SetPid {
                                    town,
                                    identity,
                                    resume_state: restored,
                                    ..
                                }) = serde_json::from_str(&txt)
                                {
                                    initial_town = town;
                                    initial_identity = identity;
                                    resume_state = restored;
                                }
                            }
                        }
                    } else {
                        prebuffer.push(Message::Text(txt));
                    }
                } else {
                    prebuffer.push(Message::Text(txt));
                }
            }
            other => prebuffer.push(other),
        }
    }

    let id = forced_id.unwrap_or_else(Uuid::new_v4);
    println!("[{}] ws new client (pre-insert) id={}", state.server_id, id);

    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    state.clients.write().await.insert(id, tx.clone());

    let identity = initial_identity.unwrap_or_else(|| match initial_town.as_ref() {
        Some(town) => GameIdentity::mayor(id, town.id.clone()),
        None => GameIdentity::debug(id),
    });
    let _ = state.ctrl_tx.send(ControlMsg::Spawn {
        id,
        town: initial_town,
        identity,
        resume_state,
    });

    let mut writer = tokio::spawn({
        let mut ws_sink = ws_sink;
        async move {
            while let Some(msg) = rx.recv().await {
                if ws_sink.send(msg).await.is_err() {
                    break;
                }
            }
        }
    });

    let hello_txt = serde_json::to_string(&serde_json::json!({
        "v": 1,
        "type": "hello",
        "id": id,
        "catalog_version": DOMAIN_VERSION
    }))
    .unwrap();
    let _ = tx.send(Message::Text(hello_txt));
    println!("[{}] hello sent to {}", state.server_id, id);

    let input_tx = state.input_tx.clone();
    let ping_sink = tx.clone();
    let ctrl_tx = state.ctrl_tx.clone();
    let mut reader = tokio::spawn(async move {
        for m in prebuffer {
            match m {
                Message::Text(txt) => {
                    let _ = handle_text(&input_tx, &ping_sink, &ctrl_tx, id, txt).await;
                }
                _ => {}
            }
        }

        while let Some(Ok(msg)) = ws_stream.next().await {
            match msg {
                Message::Text(txt) => {
                    let _ = handle_text(&input_tx, &ping_sink, &ctrl_tx, id, txt).await;
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // A failed writer is just as terminal as a closed/erroring reader. Waiting
    // only for the reader can retain a player after outbound delivery fails.
    tokio::select! {
        _ = &mut reader => {}
        _ = &mut writer => {}
    }
    reader.abort();
    writer.abort();
    let mut clients = state.clients.write().await;
    // A delayed old reader must not despawn a newly resumed connection with
    // the same player ID, or remove its outbound sender.
    if clients
        .get(&id)
        .is_some_and(|current| current.same_channel(&tx))
    {
        clients.remove(&id);
        let _ = state.ctrl_tx.send(ControlMsg::Despawn(id));
    }
}

async fn handle_text(
    input_tx: &mpsc::UnboundedSender<(ClientId, InputMsg)>,
    ping_sink: &mpsc::UnboundedSender<Message>,
    ctrl_tx: &mpsc::UnboundedSender<ControlMsg>,
    id: ClientId,
    txt: String,
) -> Result<(), ()> {
    //Accept {"v":1, ...} or legacy without "v"
    #[derive(serde::Deserialize)]
    struct MaybeVersioned<T> {
        #[allow(dead_code)]
        #[serde(default)]
        v: Option<u8>,
        #[serde(flatten)]
        inner: T,
    }

    if let Ok(wrapped) = serde_json::from_str::<MaybeVersioned<ClientMsg>>(&txt) {
        match wrapped.inner {
            ClientMsg::PauseGame => {
                let _ = ctrl_tx.send(ControlMsg::PauseGame(id));
            }
            ClientMsg::Input {
                up,
                down,
                left,
                right,
                shoot,
                ts,
            } => {
                let _ = input_tx.send((
                    id,
                    InputMsg {
                        up,
                        down,
                        left,
                        right,
                        shoot,
                        ts,
                    },
                ));
            }
            ClientMsg::Ping => {
                //reply with versioned pong
                let pong_txt = serde_json::to_string(&serde_json::json!({
                    "v": 1,
                    "type": "pong"
                }))
                .unwrap();
                let _ = ping_sink.send(Message::Text(pong_txt));
            }
        }
    }
    Ok(())
}

async fn heartbeat_task(
    state: AppState,
    http: reqwest::Client,
    conductor: String,
    registration: ServerRegister,
) {
    let mut p50 = 0.0;
    let mut p95 = 0.0;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let (players, bullets) = {
            let w = state.world.read().await;
            (w.players.len(), w.bullets.len())
        };
        let response = http
            .post(format!("{}/heartbeat", &conductor))
            .json(&Heartbeat {
                server_id: state.server_id.clone(),
                tick_ms_p50: p50,
                tick_ms_p95: p95,
                players,
                bullets,
            })
            .send()
            .await;
        // A conductor restart loses its in-memory server directory. Preserve
        // this cell's ID/address and restore registration without restarting it.
        if matches!(response, Ok(ref response) if response.status() == reqwest::StatusCode::NOT_FOUND)
        {
            let _ = http
                .post(format!("{}/register", &conductor))
                .json(&registration)
                .send()
                .await;
        }
        //You can update p50/p95 from tick metrics if you record them
        p50 = 0.9 * p50 + 0.1 * 33.0;
        p95 = 0.9 * p95 + 0.1 * 40.0;
    }
}

async fn handoff_req_consumer(state: AppState, mut sub: async_nats::Subscriber) {
    while let Some(msg) = sub.next().await {
        if let Ok(req) = serde_json::from_slice::<HandoffRequest>(&msg.payload) {
            if req.to_server != state.server_id {
                continue;
            }
            println!(
                "[{}] handoff req <- {} for player {}",
                state.server_id, req.from_server, req.player.id
            );

            let _ = state.ctrl_tx.send(ControlMsg::AdoptHandoff(req));
        }
    }
}

async fn handoff_ack_consumer(state: AppState, mut sub: async_nats::Subscriber) {
    while let Some(msg) = sub.next().await {
        if let Ok(ack) = serde_json::from_slice::<HandoffAck>(&msg.payload) {
            if ack.from_server != state.server_id {
                continue;
            }
            println!(
                "[{}] handoff ack -> {} for player {}",
                state.server_id, ack.to_server, ack.player_id
            );

            let _ = state.ctrl_tx.send(ControlMsg::AcknowledgeHandoff(ack));
        }
    }
}

struct PendingShot {
    owner: ClientId,
    x: f32,
    y: f32,
    dirx: f32,
    diry: f32,
    credit: DamageAttribution,
}

#[derive(Clone)]
struct RouteClient {
    http: reqwest::Client,
    base: String,
}

impl RouteClient {
    async fn owner_of(&self, cx: i32, cy: i32) -> anyhow::Result<String> {
        let r = self
            .http
            .get(format!("{}/route/cell/{}/{}", &self.base, cx, cy))
            .send()
            .await?
            .error_for_status()?;
        let body: RouteCellResponse = r.json().await?;
        let _target_addr = self.server_http_addr(&body.owner_server_id).await?;
        Ok(body.owner_server_id)
    }
    async fn server_http_addr(&self, server_id: &str) -> anyhow::Result<String> {
        let r = self
            .http
            .get(format!("{}/server/{}", &self.base, server_id))
            .send()
            .await?
            .error_for_status()?;
        let d: ServerDetail = r.json().await?;
        Ok(d.http_addr)
    }
}

#[derive(serde::Deserialize)]
struct ServerDetail {
    server_id: String,
    http_addr: String,
    ws_addr: String,
    capacity_hint: u32,
    last_heartbeat_ms: u128,
}
async fn game_loop(
    state: AppState,
    mut input_rx: mpsc::UnboundedReceiver<(ClientId, InputMsg)>,
    mut ctrl_rx: mpsc::UnboundedReceiver<ControlMsg>,
    http_client: reqwest::Client,
) {
    #[derive(Clone)]
    struct RouteClient {
        http: reqwest::Client,
        base: String,
    }
    impl RouteClient {
        async fn owner_of(&self, cx: i32, cy: i32) -> anyhow::Result<String> {
            let r = self
                .http
                .get(format!("{}/route/cell/{}/{}", &self.base, cx, cy))
                .send()
                .await?
                .error_for_status()?;
            let body: RouteCellResponse = r.json().await?;
            Ok(body.owner_server_id)
        }
    }
    let route = RouteClient {
        http: http_client.clone(),
        base: state.conductor.clone(),
    };

    let mut last_sim = Instant::now();
    let mut last_net = Instant::now();
    const SIM_DT: Duration = Duration::from_millis(16); //~60 Hz sim
    const NET_DT: Duration = Duration::from_millis(33); //~30 Hz network

    let player_speed = 120.0;
    let turn_speed = std::f32::consts::PI;
    let bullet_speed = 300.0;
    let muzzle = 18.0;
    let grace_ms = 80;
    let hit_r2 = 10.0f32 * 10.0f32;

    let mut event_buffer: Vec<ServerEvent> = Vec::new();

    const BULLET_DMG: i32 = 20;

    struct Win {
        cap: usize,
        data: std::collections::VecDeque<f32>,
    }
    impl Win {
        fn new(cap: usize) -> Self {
            Self {
                cap,
                data: std::collections::VecDeque::with_capacity(cap),
            }
        }
        fn push(&mut self, v_ms: f32) {
            if self.data.len() == self.cap {
                self.data.pop_front();
            }
            self.data.push_back(v_ms);
        }
        fn p(&self, q: f32) -> f32 {
            if self.data.is_empty() {
                return 0.0;
            }
            let mut v: Vec<f32> = self.data.iter().copied().collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let idx = ((v.len() - 1) as f32 * q).round() as usize;
            v[idx]
        }
    }
    let mut tick_win = Win::new(240); //~4s at 60Hz
    let mut last_hb = Instant::now();

    #[derive(Default)]
    struct PhaseMs {
        sim: f32,
        snapshot: f32,
        grids: f32,
        serialize: f32,
        send: f32,
        loop_total: f32,
    }
    let mut ph = PhaseMs::default();
    let mut tick_i: u64 = 0;

    loop {
        let loop_start = Instant::now();

        let p0 = Instant::now();
        let mut crossings: Vec<(ClientId, CellKey, PlayerState)> = Vec::new();
        let mut sim_events: Vec<ServerEvent> = Vec::new();
        let mut handoff_acks = Vec::new();
        let mut switches = Vec::new();
        let mut private_messages = Vec::new();
        if last_sim.elapsed() >= SIM_DT {
            let dt = last_sim.elapsed().as_secs_f32().clamp(0.0001, 0.1);
            last_sim = Instant::now();

            {
                let mut w = state.world.write().await;

                while let Ok(ctrl) = ctrl_rx.try_recv() {
                    match ctrl {
                        ControlMsg::PrepareConstruction { preparation, reply } => {
                            let error = w.prepare_construction(preparation, &state.server_id).err();
                            let _ = reply.send(error);
                        }
                        ControlMsg::ApplyEconomy {
                            projection,
                            requested_at,
                        } => {
                            w.apply_economy(projection, &state.server_id, requested_at);
                        }
                        ControlMsg::Spawn {
                            id,
                            town,
                            identity,
                            resume_state,
                        } => {
                            if w.ended_games.contains(&identity.game_id) {
                                private_messages.push((id,serde_json::json!({"type":"game_ended","message":"This town is no longer available."}).to_string()));
                            } else {
                                if let Some(restored) = resume_state.filter(|state| {
                                    state.id == id && state.game.identity == identity
                                }) {
                                    w.adopt_handoff(restored);
                                }
                                if !w.attach_player(id, town, identity) {
                                    w.remove_player(id);
                                    private_messages.push((id,serde_json::json!({"type":"game_ended","message":"This town allocation has ended or been superseded."}).to_string()));
                                }
                            }
                        }
                        ControlMsg::Despawn(id) => w.remove_player(id),
                        ControlMsg::SyncTowns {
                            towns,
                            requested_at,
                        } => w.sync_towns(towns, requested_at),
                        ControlMsg::EndGame(event) => {
                            let ids: Vec<_> = w
                                .players
                                .iter()
                                .filter(|(_, p)| p.game.identity.game_id == event.player_id)
                                .map(|(id, _)| *id)
                                .collect();
                            w.end_game(event.player_id);
                            for id in ids {
                                private_messages.push((id,serde_json::json!({"type":"game_ended","message":"Your town's game has ended.","reason":event.reason}).to_string()));
                            }
                        }
                        ControlMsg::PauseGame(id) => {
                            if let Some(player) = w.players.get(&id) {
                                private_messages.push((id,serde_json::json!({"type":"game_checkpoint","state":player.handoff_state(id)}).to_string()));
                            }
                            w.remove_player(id);
                        }
                        ControlMsg::InsertBuilding { building, reply } => {
                            let _ = reply.send(w.insert_building(building).map_err(str::to_owned));
                        }
                        ControlMsg::RemoveBuilding { id, game_id, reply } => {
                            let _ =
                                reply.send(w.remove_building(id, game_id).map_err(str::to_owned));
                        }
                        ControlMsg::AdoptHandoff(request) => {
                            let id = request.player.id;
                            let accepted = w.adopt_handoff(request.player);
                            handoff_acks.push(HandoffAck {
                                to_server: state.server_id.clone(),
                                from_server: request.from_server,
                                player_id: id,
                                accepted,
                                ts_ms: now_millis(),
                            });
                        }
                        ControlMsg::AcknowledgeHandoff(ack) => {
                            if w.complete_handoff(&ack) {
                                switches.push((ack.player_id, ack.to_server));
                            }
                        }
                        ControlMsg::ControlSpawnAt { player_id, tile } => {
                            if let Some(player) = w.players.get_mut(&player_id) {
                                (player.x, player.y) = player_spawn_point(player_id, Some(tile));
                                player.spawn_tile = Some(tile);
                            }
                        }
                    }
                }

                while let Ok((id, inp)) = input_rx.try_recv() {
                    if let Some(s) = w.last_input.get_mut(&id) {
                        *s = InputState {
                            up: inp.up,
                            down: inp.down,
                            left: inp.left,
                            right: inp.right,
                            shoot: inp.shoot,
                        };
                    }
                }

                let ids: Vec<ClientId> = w.players.keys().copied().collect();
                let mut pending_shots: Vec<PendingShot> = Vec::new();

                for id in ids {
                    let s = w.last_input.get(&id).cloned().unwrap_or_default();
                    let prev_shoot = *w.last_shoot.get(&id).unwrap_or(&false);

                    if let Some(p) = w.players.get_mut(&id) {
                        if p.transferring || p.economy_pending {
                            continue;
                        }
                        let alive = p.hp > 0;

                        let rot_dir = (s.right as i8 as f32) - (s.left as i8 as f32);
                        if alive {
                            p.ang += rot_dir * turn_speed * dt;
                            if p.ang > std::f32::consts::PI {
                                p.ang -= 2.0 * std::f32::consts::PI;
                            }
                            if p.ang < -std::f32::consts::PI {
                                p.ang += 2.0 * std::f32::consts::PI;
                            }
                        }

                        let fwd = if alive {
                            (s.up as i8 as f32) - (s.down as i8 as f32)
                        } else {
                            0.0
                        };
                        if fwd.abs() > 0.0 {
                            let dirx = p.ang.cos();
                            let diry = p.ang.sin();
                            p.vx = dirx * player_speed * fwd;
                            p.vy = diry * player_speed * fwd;
                        } else {
                            p.vx = 0.0;
                            p.vy = 0.0;
                        }

                        if alive && s.shoot && !prev_shoot {
                            let dirx = p.ang.cos();
                            let diry = p.ang.sin();
                            let x = p.x + dirx * muzzle;
                            let y = p.y + diry * muzzle;
                            pending_shots.push(PendingShot {
                                owner: id,
                                x,
                                y,
                                dirx,
                                diry,
                                credit: DamageAttribution {
                                    source_entity_id: id,
                                    source_kind: DamageSourceKind::Player,
                                    beneficiary: Some(p.game.identity.combatant(id)),
                                },
                            });
                        }
                    }

                    w.last_shoot.insert(id, s.shoot);
                }

                let ids_for_collision: Vec<ClientId> = w.players.keys().copied().collect();
                for id in ids_for_collision {
                    let (combatant, mut nx, mut ny) = match w.players.get_mut(&id) {
                        Some(p) => {
                            if p.hp <= 0 || p.transferring || p.economy_pending {
                                p.vx = 0.0;
                                p.vy = 0.0;
                                continue;
                            }
                            (
                                p.game.identity.combatant(id),
                                p.x + p.vx * dt,
                                p.y + p.vy * dt,
                            )
                        }
                        None => continue,
                    };

                    resolve_circle_vs_tiles(&mut w, &mut nx, &mut ny, PLAYER_RADIUS);

                    resolve_vs_enemy_players(id, &combatant, &mut nx, &mut ny, &w.players);
                    w.buildings.push_circle(&mut nx, &mut ny, PLAYER_RADIUS);

                    if let Some(p) = w.players.get_mut(&id) {
                        p.x = nx;
                        p.y = ny;
                    }
                }
                for b in w.bullets.values_mut() {
                    b.previous_x = b.x;
                    b.previous_y = b.y;
                    b.x += b.vx * dt;
                    b.y += b.vy * dt;
                }

                {
                    let samples: Vec<(BulletId, f32, f32)> =
                        w.bullets.iter().map(|(bid, b)| (*bid, b.x, b.y)).collect();

                    let mut to_kill: Vec<BulletId> = Vec::new();
                    for (bid, bx, by) in samples {
                        let t = tile_at_world(&mut w, bx, by);
                        let bullet = &w.bullets[&bid];
                        let town = bullet
                            .credit
                            .beneficiary
                            .as_ref()
                            .and_then(|owner| owner.town_id.as_deref());
                        if t.blocks_bullet()
                            || w.buildings.blocks_segment(
                                (bullet.previous_x, bullet.previous_y),
                                (bx, by),
                                town,
                            )
                        {
                            to_kill.push(bid);
                        }
                    }
                    for bid in to_kill {
                        w.bullets.remove(&bid);
                    }
                }

                for PendingShot {
                    owner,
                    x,
                    y,
                    dirx,
                    diry,
                    credit,
                } in pending_shots
                {
                    w.next_bullet_id += 1;
                    let bid = w.next_bullet_id;
                    w.bullets.insert(
                        bid,
                        Bullet {
                            id: bid,
                            x,
                            y,
                            vx: dirx * bullet_speed,
                            vy: diry * bullet_speed,
                            owner,
                            credit,
                            born_ms: now_millis(),
                            previous_x: x,
                            previous_y: y,
                        },
                    );
                }

                w.bullets
                    .retain(|_, b| b.x.abs() < 10000.0 && b.y.abs() < 10000.0);

                let nowms = now_millis();
                let mut dead_bullets: Vec<BulletId> = Vec::new();

                let bullets_for_hit: Vec<(BulletId, DamageAttribution, f32, f32, u64)> = w
                    .bullets
                    .iter()
                    .map(|(bid, b)| (*bid, b.credit.clone(), b.x, b.y, b.born_ms))
                    .collect();

                for (bid, credit, bx, by, born) in bullets_for_hit {
                    if nowms.saturating_sub(born) < grace_ms {
                        continue;
                    }

                    for (pid, p) in w.players.iter_mut() {
                        if p.hp <= 0
                            || p.transferring
                            || !credit.can_damage(&p.game.identity.combatant(*pid))
                        {
                            continue;
                        }

                        let dx = bx - p.x;
                        let dy = by - p.y;
                        if dx * dx + dy * dy <= hit_r2 {
                            sim_events.extend(p.take_damage(*pid, BULLET_DMG, &credit, nowms));
                            dead_bullets.push(bid);
                            break;
                        }
                    }
                }
                for bid in dead_bullets {
                    w.bullets.remove(&bid);
                }

                for (pid, p) in w.players.iter_mut() {
                    if p.hp <= 0 && !p.transferring && !p.economy_pending {
                        if let Some(t) = p.respawn_at {
                            if nowms >= t {
                                let (sx, sy) = player_spawn_point(*pid, p.spawn_tile);
                                p.x = sx;
                                p.y = sy;
                                p.vx = 0.0;
                                p.vy = 0.0;
                                p.hp = p.max_hp;
                                p.respawn_at = None;
                                sim_events.push(ServerEvent::Respawn {
                                    id: *pid,
                                    x: p.x,
                                    y: p.y,
                                });
                            }
                        }
                    }
                }

                for (id, p) in w.players.iter_mut() {
                    let ck = cell_of(p.x, p.y, CELL_W);
                    let retry = p
                        .transfer_started_at_ms
                        .is_some_and(|started| nowms.saturating_sub(started) >= 5000);
                    if ck != p.last_cell && (!p.transferring || retry) && !p.economy_pending {
                        crossings.push((*id, ck, p.handoff_state(*id)));
                        p.transferring = true;
                        p.transfer_started_at_ms = Some(nowms);
                    } else if !p.transferring {
                        p.last_cell = ck;
                    }
                }
            }
        }

        event_buffer.extend(sim_events.drain(..));
        for (id, text) in private_messages {
            if let Some(sender) = state.clients.read().await.get(&id) {
                let _ = sender.send(Message::Text(text));
            }
        }

        for ack in handoff_acks {
            let _ = state
                .nats
                .publish(
                    format!("handoff.{}.ack", ack.from_server),
                    serde_json::to_vec(&ack).unwrap().into(),
                )
                .await;
        }
        for (id, to_server) in switches {
            let _ = state
                .nats
                .publish(
                    format!("switch.{}", id),
                    serde_json::json!({"to_server":to_server})
                        .to_string()
                        .into(),
                )
                .await;
        }

        ph.sim = 1000.0 * p0.elapsed().as_secs_f32();

        if !crossings.is_empty() {
            for (pid, ck, pstate) in crossings.drain(..) {
                let owner = match route.owner_of(ck.0, ck.1).await {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[{}] route owner err: {}", state.server_id, e);
                        if let Some(player) = state.world.write().await.players.get_mut(&pid) {
                            player.transferring = false;
                        }
                        continue;
                    }
                };
                if owner != state.server_id {
                    if let Some(player) = state.world.write().await.players.get_mut(&pid) {
                        player.transfer_target = Some(owner.clone());
                    }
                    let req = HandoffRequest {
                        from_server: state.server_id.clone(),
                        to_server: owner.clone(),
                        player: pstate.clone(),
                        ts_ms: now_millis(),
                    };
                    println!(
                        "[{}] -> handoff {} to {} at cell ({},{})",
                        state.server_id, req.player.id, owner, ck.0, ck.1
                    );
                    if state
                        .nats
                        .publish(
                            format!("handoff.{}.req", owner),
                            serde_json::to_vec(&req).unwrap().into(),
                        )
                        .await
                        .is_err()
                    {
                        if let Some(player) = state.world.write().await.players.get_mut(&pid) {
                            player.transferring = false;
                            player.transfer_target = None;
                            player.transfer_started_at_ms = None;
                        }
                    }
                } else {
                    let mut w = state.world.write().await;
                    if let Some(p) = w.players.get_mut(&pid) {
                        p.last_cell = ck;
                        p.transferring = false;
                        p.transfer_target = None;
                        p.transfer_started_at_ms = None;
                    }
                }
            }
        }

        if last_net.elapsed() >= NET_DT {
            last_net = Instant::now();

            let p1 = Instant::now();
            let (players_snapshot, bullets_snapshot, towns_snapshot, game_states, resume_states) = {
                let w = state.world.read().await;
                let players_snapshot: Vec<(ClientId, f32, f32, f32, i32)> = w
                    .players
                    .iter()
                    .map(|(id, p)| (*id, p.x, p.y, p.ang, p.hp))
                    .collect();
                let bullets_snapshot: Vec<Bullet> = w.bullets.values().cloned().collect();
                let game_states: HashMap<ClientId, PlayerGameState> = w
                    .players
                    .iter()
                    .map(|(id, p)| (*id, p.game.clone()))
                    .collect();
                let resume_states: HashMap<ClientId, PlayerState> = w
                    .players
                    .iter()
                    .map(|(id, p)| (*id, p.handoff_state(*id)))
                    .collect();
                (
                    players_snapshot,
                    bullets_snapshot,
                    w.towns.clone(),
                    game_states,
                    resume_states,
                )
            };
            ph.snapshot = 1000.0 * p1.elapsed().as_secs_f32();

            let p3 = Instant::now();
            let players_json = serde_json::to_string(&players_snapshot).unwrap();
            let bullets_json = serde_json::to_string(&bullets_snapshot).unwrap();
            let towns_json = serde_json::to_string(&towns_snapshot).unwrap();
            let affiliations: Vec<PlayerAffiliation> = game_states
                .iter()
                .map(|(id, game)| PlayerAffiliation {
                    player_id: *id,
                    town_id: game.identity.town_id.clone(),
                    role: game.identity.role,
                })
                .collect();
            let affiliations_json = serde_json::to_string(&affiliations).unwrap();

            let events_to_send: Vec<ServerEvent> = std::mem::take(&mut event_buffer);
            let events_json = serde_json::to_string(&events_to_send).unwrap();

            let tms = now_millis();
            ph.serialize = 1000.0 * p3.elapsed().as_secs_f32();

            let p4 = Instant::now();
            let clients_snapshot = state.clients.read().await.clone();

            use std::collections::HashMap;
            let mut pmap: HashMap<ClientId, (f32, f32, f32, i32)> =
                HashMap::with_capacity(players_snapshot.len());
            for (pid, x, y, ang, hp) in &players_snapshot {
                pmap.insert(*pid, (*x, *y, *ang, *hp));
            }

            const VIEW_W: i32 = 26;
            const VIEW_H: i32 = 18;

            for (id, ch) in clients_snapshot {
                let Some(game) = game_states.get(&id) else {
                    continue;
                };
                let self_json = serde_json::to_string(game).unwrap();
                let (px, py) = if let Some((x, y, _, _)) = pmap.get(&id) {
                    (*x, *y)
                } else {
                    (0.0, 0.0)
                };

                let (ptx, pty) = world_to_tile(px, py);
                let ox = ptx - VIEW_W / 2;
                let oy = pty - VIEW_H / 2;

                let (tiles_json, buildings_json) = {
                    let mut w = state.world.write().await;
                    let window = collect_tile_window(&mut w, ox, oy, VIEW_W, VIEW_H);
                    let buildings = w.visible_buildings(WorldRect {
                        x: ox as f32 * TILE_PX,
                        y: oy as f32 * TILE_PX,
                        width: VIEW_W as f32 * TILE_PX,
                        height: VIEW_H as f32 * TILE_PX,
                    });
                    (
                        serde_json::to_string(&window).unwrap(),
                        serde_json::to_string(&buildings).unwrap(),
                    )
                };

                let txt = format!(
                    r#"{{"v":1,"type":"state","t":{},"me":"{}","players":{},"bullets":{},"tiles":{},"towns":{},"catalog_version":{},"self_state":{},"player_affiliations":{},"events":{},"buildings":{},"resume_state":{},"construction":{}}}"#,
                    tms,
                    id,
                    players_json,
                    bullets_json,
                    tiles_json,
                    towns_json,
                    DOMAIN_VERSION,
                    self_json,
                    affiliations_json,
                    events_json,
                    buildings_json,
                    serde_json::to_string(&resume_states[&id]).unwrap(),
                    serde_json::to_string(&state.world.read().await.construction_status(id))
                        .unwrap()
                );

                let _ = ch.send(Message::Text(txt));
            }
            ph.send = 1000.0 * p4.elapsed().as_secs_f32();

            if tick_i % 60 == 0 {
                println!(
                    "[{}] ms sim={:.1} snap={:.1} ser={:.1} send={:.1} p50={:.1} p95={:.1}",
                    state.server_id,
                    ph.sim,
                    ph.snapshot,
                    ph.serialize,
                    ph.send,
                    tick_win.p(0.50),
                    tick_win.p(0.95)
                );
            }
            tick_i += 1;
        }

        let frame = loop_start.elapsed();
        ph.loop_total = 1000.0 * frame.as_secs_f32();
        tick_win.push(ph.loop_total);

        if last_hb.elapsed() >= Duration::from_secs(2) {
            last_hb = Instant::now();
            let (players_cnt, bullets_cnt) = {
                let w = state.world.read().await;
                (w.players.len(), w.bullets.len())
            };
            let hb = twz_shared::Heartbeat {
                server_id: state.server_id.clone(),
                tick_ms_p50: tick_win.p(0.50),
                tick_ms_p95: tick_win.p(0.95),
                players: players_cnt,
                bullets: bullets_cnt,
            };

            let _ = http_client
                .post(format!("{}/heartbeat", &state.conductor))
                .json(&hb)
                .send()
                .await;
        }

        if frame < SIM_DT {
            tokio::time::sleep(SIM_DT - frame).await;
        }
    }
}

fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

pub fn health_router() -> Router {
    Router::new().route("/healthz", get(|| async { "ok" }))
}

#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod spawn_tests {
    use super::*;

    #[test]
    fn mayor_spawn_and_respawn_follow_the_allocated_hall() {
        for slot in [0, 1, 4, 20] {
            let hall = twz_shared::hall_for_slot(slot);
            let tile = twz_shared::spawn_from_hall(hall.origin);
            assert!(matches!(
                terrain_at_tile(hall.origin.x, hall.origin.y),
                Tile::Ground
            ));
            let mut world = World::default();
            get_chunk(&mut world, hall.cell.0, hall.cell.1);
            assert!(world
                .buildings
                .buildings
                .contains_key(&twz_shared::hall_entity_id(hall.origin)));
            assert!(matches!(terrain_at_tile(tile.x, tile.y), Tile::Ground));
            let (x, y) = player_spawn_point(Uuid::new_v4(), Some(tile));
            assert_eq!(world_to_tile(x, y), (tile.x, tile.y));
            assert_eq!(cell_of(x, y, CELL_W), hall.cell);
        }
    }

    #[test]
    fn connections_and_respawns_use_ground_below_a_generated_hall() {
        assert!(matches!(
            terrain_at_tile(TOWN_OFFSET, TOWN_OFFSET),
            Tile::Ground
        ));
        let expected = twz_shared::spawn_from_hall(IVec2 {
            x: TOWN_OFFSET,
            y: TOWN_OFFSET,
        });
        for id in [Uuid::nil(), Uuid::new_v4()] {
            let (x, y) = spawn_point_for(id);
            assert_eq!(
                (x, y),
                (expected.x as f32 * TILE_PX, expected.y as f32 * TILE_PX)
            );
            assert_eq!(world_to_tile(x, y), (expected.x, expected.y));
            assert!(matches!(
                terrain_at_tile(expected.x, expected.y),
                Tile::Ground
            ));
            // The existing player collision pass must not displace this spawn.
            let mut world = World::default();
            let (mut resolved_x, mut resolved_y) = (x, y);
            resolve_circle_vs_tiles(&mut world, &mut resolved_x, &mut resolved_y, PLAYER_RADIUS);
            assert_eq!((resolved_x, resolved_y), (x, y));
        }
    }
}

#[cfg(test)]
mod cell_boundary_tests {
    use super::*;

    #[test]
    fn routing_cells_span_64_tiles_on_both_axes() {
        let boundary = 64.0 * TILE_PX;
        assert_eq!(CELL_W, boundary);
        assert_eq!(cell_of(0.0, 0.0, CELL_W), (0, 0));
        assert_eq!(cell_of(boundary - 1.0, boundary - 1.0, CELL_W), (0, 0));
        assert_eq!(cell_of(boundary, 0.0, CELL_W), (1, 0));
        assert_eq!(cell_of(0.0, boundary, CELL_W), (0, 1));
        assert_eq!(cell_of(-1.0, -1.0, CELL_W), (-1, -1));
        assert_eq!(cell_of(-boundary, -boundary, CELL_W), (-1, -1));
        assert_eq!(cell_of(-boundary - 1.0, -boundary - 1.0, CELL_W), (-2, -2));
    }
}
