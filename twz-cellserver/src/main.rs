use twz_shared::{
    //spawn + town glue
    SpawnPlayerAt, IVec2, CellKey, ClientId, ServerRegister,
    Heartbeat, HandoffRequest, HandoffAck, RouteCellResponse, PlayerState, cell_of,
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
use tracing::{info};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpListener, sync::{mpsc, RwLock}};
use uuid::Uuid;

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
    Hit   { src: ClientId, dst: ClientId, dmg: i32, hp: i32 },
    Death { id: ClientId },
    Respawn { id: ClientId, x: f32, y: f32 },
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
    max_hp: i32,
    respawn_at: Option<u64>,
    team: u8,      
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
}

#[derive(Default, Clone, Serialize)]
struct World {
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
    terrain_chunks: HashMap<(i32,i32), Vec<u8>>, //each chunk is CHUNK_TILES*CHUNK_TILES bytes
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

#[derive(Debug, Clone)]
enum ControlMsg {
    Spawn(ClientId),
    Despawn(ClientId),
    ControlSpawnAt { player_id: ClientId, tile: IVec2 },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMsg<'a> {
    Hello { id: ClientId },
    State {
        t: u64,
        me: ClientId,
        players: Vec<(ClientId, f32, f32, f32, i32)>,
        bullets: Vec<&'a Bullet>,
    },
    Pong,
}

const TILE_PX: f32 = 48.0;          //each tile is 48×48 pixels
const CHUNK_TILES: i32 = 64;        //64×64 tiles per cached chunk (tweakable)
const TOWN_GRID: i32 = 32;          //512/16 = 32 tiles between town halls (16×16 sub-grid)
const TOWN_OFFSET: i32 = TOWN_GRID / 2;
const TOWN_CLEAR_RAD: i32 = 4;      //tiles of guaranteed ground around a town hall
const TERRAIN_SEED: u64 = 0xC0FFEE_BABE_5EED; //change to remix the world

const TILE: f32 = TILE_PX;          //48.0 px tiles (you already have TILE_PX)
const PLAYER_RADIUS: f32 = 16.0;     //tweak to taste
const MAX_PENETRATION_ITER: usize = 3;

impl Tile {
    #[inline] fn blocks_player(self) -> bool {
        matches!(self, Tile::Water | Tile::Rock | Tile::Town)
    }
    #[inline] fn blocks_bullet(self) -> bool {
        matches!(self, Tile::Rock | Tile::Town)
    }
}

#[inline] fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    if v < lo { lo } else if v > hi { hi } else { v }
}

fn resolve_circle_vs_tiles(world: &mut World, x: &mut f32, y: &mut f32, r: f32) {
    //check a small neighborhood of tiles; iterate a few times in case of multiple overlaps
    for _ in 0..MAX_PENETRATION_ITER {
        let mut pushed = false;

        let min_tx = (( *x - r) / TILE).floor() as i32;
        let max_tx = (((*x + r) / TILE).floor() as i32) + 1;
        let min_ty = (( *y - r) / TILE).floor() as i32;
        let max_ty = (((*y + r) / TILE).floor() as i32) + 1;

        'tileloop: for ty in min_ty..=max_ty {
            for tx in min_tx..=max_tx {
                let t = tile_at(world, tx, ty);
                if !t.blocks_player() { continue; }

                let rx0 = tx as f32 * TILE;
                let ry0 = ty as f32 * TILE;
                let rx1 = rx0 + TILE;
                let ry1 = ry0 + TILE;

                let cx = clamp(*x, rx0, rx1);
                let cy = clamp(*y, ry0, ry1);

                let dx = *x - cx;
                let dy = *y - cy;
                let d2 = dx*dx + dy*dy;

                if d2 < r*r {
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
        if !pushed { break; }
    }
}

fn resolve_vs_enemy_players(
    me_id: ClientId,
    me_team: u8,
    x: &mut f32,
    y: &mut f32,
    world_players: &std::collections::HashMap<ClientId, Player>,
) {
    for (pid, other) in world_players.iter() {
        if *pid == me_id { continue; }
        if other.team == me_team { continue; } //friendly pass-through
        if other.hp <= 0 { continue; }         //ignore dead bodies

        let dx = *x - other.x;
        let dy = *y - other.y;
        let d2 = dx*dx + dy*dy;
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
    Water  = 1,
    Rock   = 2,
    Town   = 3,
}
impl Tile {
    #[inline] fn is_solid(self) -> bool {
        matches!(self, Tile::Water | Tile::Rock)
    }
}

//World coordinate <-> tile/chunk helpers
#[inline] fn world_to_tile(x: f32, y: f32) -> (i32, i32) {
    let tx = (x / TILE_PX).floor() as i32;
    let ty = (y / TILE_PX).floor() as i32;
    (tx, ty)
}
#[inline] fn tile_to_chunk(tx: i32, ty: i32) -> (i32, i32) {
    let cx = div_floor(tx, CHUNK_TILES);
    let cy = div_floor(ty, CHUNK_TILES);
    (cx, cy)
}
#[inline] fn div_floor(a: i32, b: i32) -> i32 {
    let d = a / b;
    let r = a % b;
    if (r != 0) && ((r < 0) != (b < 0)) { d - 1 } else { d }
}
#[inline] fn pos_in_chunk(tx: i32, ty: i32) -> (i32, i32) {
    let (cx, cy) = tile_to_chunk(tx, ty);
    (tx - cx * CHUNK_TILES, ty - cy * CHUNK_TILES)
}

//simple 2D hash -> [0,1)
#[inline] fn hash2(x: i32, y: i32, seed: u64) -> f32 {
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

#[inline] fn lerp(a: f32, b: f32, t: f32) -> f32 { a + (b - a) * t }

#[inline] fn smoothstep(t: f32) -> f32 {
    //smoother interpolation
    t * t * (3.0 - 2.0 * t)
}

//value noise with bilinear smoothing at integer lattice
fn value_noise(x: f32, y: f32, seed: u64) -> f32 {
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = x - x.floor();
    let fy = y - y.floor();

    let v00 = hash2(x0,     y0,     seed);
    let v10 = hash2(x0 + 1, y0,     seed);
    let v01 = hash2(x0,     y0 + 1, seed);
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
    let mut freq = 1.0 / 12.0;  //base period ~12 tiles
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
fn is_town_tile(tx: i32, ty: i32) -> bool {
    (tx - TOWN_OFFSET).rem_euclid(TOWN_GRID) == 0 &&
    (ty - TOWN_OFFSET).rem_euclid(TOWN_GRID) == 0
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
    if is_town_tile(tx, ty) {
        return Tile::Town;
    }
    if near_town_clear(tx, ty) {
        return Tile::Ground;
    }

    let w_n = fbm(tx as f32, ty as f32, TERRAIN_SEED ^ 0xA5A5A5A5);
    let r_n = fbm(tx as f32, ty as f32, TERRAIN_SEED ^ 0x5A5A5A5A);

    if w_n < 0.35 { return Tile::Water; }
    if r_n > 0.68 { return Tile::Rock; }
    Tile::Ground


}#[derive(serde::Serialize)]
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
            data.push(terrain_at_tile(tx, ty) as u8);
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
    world.terrain_chunks.entry((cx, cy)).or_insert_with(|| generate_chunk(cx, cy))
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
        3 => Tile::Town,
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
            if h.is_empty() { None } else { Some(h.to_string()) }
        }
        Err(_) => None,
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
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

    {
        let nats = state.nats.clone();
        tokio::spawn(async move {
            let mut sub = match nats.subscribe("twz.cell.*.*.spawn").await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("spawn subscribe failed: {e}");
                    return;
                }
            };

            while let Some(msg) = sub.next().await {
                match serde_json::from_slice::<twz_shared::SpawnPlayerAt>(&msg.payload) {
                    Ok(cmd) => {
                        tracing::info!("spawn request: player {:?} at tile {:?}", cmd.player_id, cmd.tile);
                        
                        let player_id = cmd.player_id.clone();
                        let tile = cmd.tile;

                        if let Err(e) = spawn_player_at(&player_id, tile).await {
                            tracing::warn!("spawn failed: {e}");
                        }
                    }
                    Err(e) => tracing::warn!("bad SpawnPlayerAt: {e}"),
                }
            }
        });
    }

    {
        let mut w = state.world.write().await;
        let c = get_chunk(&mut w, 0, 0);
        println!("chunk (0,0) first 32 tiles: {:?}", &c[0..32.min(c.len())]);
    }

    //register
    let advertise_addr = args.advertise.clone()
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
    http_client
        .post(format!("{}/register", &args.conductor))
        .json(&ServerRegister {
            server_id: args.id.clone(),
            http_addr: format!("http://{}", advertise_addr),
            ws_addr: format!("ws://{}", advertise_addr),
            capacity_hint: 1000,
        })
        .send()
        .await?
        .error_for_status()?;

    tokio::spawn(heartbeat_task(
        state.clone(),
        http_client.clone(),
        args.conductor.clone(),
    ));

    tokio::spawn(game_loop(state.clone(), input_rx, ctrl_rx, http_client.clone()));

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

fn spawn_point_for(id: ClientId) -> (f32, f32) {
    let bytes = id.as_bytes();
    let hash = u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5], bytes[6], bytes[7],
    ]);
    
    let ang = ((hash as f32) * 2.328_306_5e-10) % (std::f32::consts::PI * 2.0);

    let r = 200.0;
    (ang.cos() * r, ang.sin() * r)
}

async fn ws_handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| client_socket(state, socket))
}

async fn client_socket(state: AppState, socket: WebSocket) {
    let (ws_sink, mut ws_stream) = socket.split();

    let mut forced_id: Option<Uuid> = None;
    let mut prebuffer: Vec<Message> = Vec::new();

    if let Some(Ok(msg)) = ws_stream.next().await {
        match msg {
            Message::Text(txt) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                    if v.get("type").and_then(|t| t.as_str()) == Some("set_pid") {
                        if let Some(s) = v.get("id").and_then(|idv| idv.as_str()) {
                            if let Ok(parsed) = Uuid::parse_str(s) {
                                forced_id = Some(parsed);
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

    let _ = state.ctrl_tx.send(ControlMsg::Spawn(id));

    let writer = tokio::spawn({
        let mut ws_sink = ws_sink;
        async move {
            while let Some(msg) = rx.recv().await {
                if ws_sink.send(msg).await.is_err() { break; }
            }
        }
    });

    let hello_txt = serde_json::to_string(&serde_json::json!({
        "v": 1,
        "type": "hello",
        "id": id
    })).unwrap();
    let _ = tx.send(Message::Text(hello_txt));
    println!("[{}] hello sent to {}", state.server_id, id);

    let input_tx = state.input_tx.clone();
    let ping_sink = tx.clone();
    let reader = tokio::spawn(async move {
        for m in prebuffer {
            match m {
                Message::Text(txt) => {
                    let _ = handle_text(&input_tx, &ping_sink, id, txt).await;
                }
                _ => {}
            }
        }

        while let Some(Ok(msg)) = ws_stream.next().await {
            match msg {
                Message::Text(txt) => {
                    let _ = handle_text(&input_tx, &ping_sink, id, txt).await;
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    let _ = reader.await;
    {
        state.clients.write().await.remove(&id);
        let mut w = state.world.write().await;
        w.players.remove(&id);
        w.last_input.remove(&id);
        w.last_shoot.remove(&id);
    }
    writer.abort();

    state.clients.write().await.remove(&id);
    let _ = state.ctrl_tx.send(ControlMsg::Despawn(id));
}

async fn handle_text(
    input_tx: &mpsc::UnboundedSender<(ClientId, InputMsg)>,
    ping_sink: &mpsc::UnboundedSender<Message>,
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
            ClientMsg::Input { up, down, left, right, shoot, ts } => {
                let _ = input_tx.send((id, InputMsg { up, down, left, right, shoot, ts }));
            }
            ClientMsg::Ping => {
                //reply with versioned pong
                let pong_txt = serde_json::to_string(&serde_json::json!({
                    "v": 1,
                    "type": "pong"
                })).unwrap();
                let _ = ping_sink.send(Message::Text(pong_txt));
            }
        }
    }
    Ok(())
}

async fn heartbeat_task(state: AppState, http: reqwest::Client, conductor: String) {
    let mut p50 = 0.0;
    let mut p95 = 0.0;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let (players, bullets) = {
            let w = state.world.read().await;
            (w.players.len(), w.bullets.len())
        };
        let _ = http
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
        //You can update p50/p95 from tick metrics if you record them
        p50 = 0.9 * p50 + 0.1 * 33.0;
        p95 = 0.9 * p95 + 0.1 * 40.0;
    }
}

async fn handoff_req_consumer(state: AppState, mut sub: async_nats::Subscriber) {
    while let Some(msg) = sub.next().await {
        if let Ok(req) = serde_json::from_slice::<HandoffRequest>(&msg.payload) {
            if req.to_server != state.server_id { continue; }
            println!("[{}] handoff req <- {} for player {}", state.server_id, req.from_server, req.player.id);

            {
                let mut w = state.world.write().await;
                w.players.insert(req.player.id, Player {
                    x: req.player.x,
                    y: req.player.y,
                    vx: req.player.vx,
                    vy: req.player.vy,
                    ang: req.player.ang,
                    hp: req.player.hp,
                    max_hp: req.player.max_hp,
                    respawn_at: req.player.respawn_at,
                    last_cell: cell_of(req.player.x, req.player.y, 64.0),
                    transferring: false,
                    team: req.player.team,
                });
                w.last_input.insert(req.player.id, Default::default());
                w.last_shoot.insert(req.player.id, false);
            }

            let ack = HandoffAck {
                to_server: state.server_id.clone(),
                from_server: req.from_server.clone(),
                player_id: req.player.id,
                accepted: true,
                ts_ms: now_millis(),
            };
            let _ = state
                .nats
                .publish(format!("handoff.{}.ack", req.from_server), serde_json::to_vec(&ack).unwrap().into())
                .await;
        }
    }
}

async fn handoff_ack_consumer(state: AppState, mut sub: async_nats::Subscriber) {
    while let Some(msg) = sub.next().await {
        if let Ok(ack) = serde_json::from_slice::<HandoffAck>(&msg.payload) {
            if ack.from_server != state.server_id { continue; }
            println!("[{}] handoff ack -> {} for player {}", state.server_id, ack.to_server, ack.player_id);

            {
                let mut w = state.world.write().await;
                w.players.remove(&ack.player_id);
                w.last_input.remove(&ack.player_id);
                w.last_shoot.remove(&ack.player_id);
            }

            let _ = state.nats
                .publish(
                    format!("switch.{}", ack.player_id),
                    serde_json::to_vec(&serde_json::json!({"to_server": ack.to_server})).unwrap().into(),
                )
                .await;
        }
    }
}

struct PendingShot {
    owner: ClientId,
    x: f32,
    y: f32,
    dirx: f32,
    diry: f32,
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
        let target_addr = self.server_http_addr(&body.owner_server_id).await?;
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
fn tile_to_world(t: IVec2) -> glam::Vec3 {
    const TILE_SIZE: f32 = 1.0;
    glam::Vec3::new(t.x as f32 * TILE_SIZE, 0.0, t.y as f32 * TILE_SIZE)
}

async fn spawn_player_at(player_id: &twz_shared::ClientId, tile: twz_shared::IVec2) -> anyhow::Result<()> {
    const TILE_SIZE: f32 = 1.0;
    let world = glam::Vec3::new(tile.x as f32 * TILE_SIZE, 0.0, tile.y as f32 * TILE_SIZE);
    info!("(noop) spawn player {:?} at tile {:?} (world {:?}", player_id, tile, world);
    Ok(())
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
            let r = self.http
                .get(format!("{}/route/cell/{}/{}", &self.base, cx, cy))
                .send().await?
                .error_for_status()?;
            let body: RouteCellResponse = r.json().await?;
            Ok(body.owner_server_id)
        }
    }
    let route = RouteClient { http: http_client.clone(), base: state.conductor.clone() };

    let mut last_sim = Instant::now();
    let mut last_net = Instant::now();
    const SIM_DT: Duration = Duration::from_millis(16);  //~60 Hz sim
    const NET_DT: Duration = Duration::from_millis(33);  //~30 Hz network

    let player_speed = 120.0;
    let turn_speed   = std::f32::consts::PI;
    let bullet_speed = 300.0;
    let muzzle       = 18.0;
    let grace_ms     = 80;
    let hit_r2       = 10.0f32 * 10.0f32;

    let mut event_buffer: Vec<ServerEvent> = Vec::new();

    const BULLET_DMG: i32 = 20;
    const RESPAWN_MS: u64 = 3000;

    const CELL_W: f32 = 64.0;

    struct Win {
        cap: usize,
        data: std::collections::VecDeque<f32>,
    }
    impl Win {
        fn new(cap: usize) -> Self {
            Self { cap, data: std::collections::VecDeque::with_capacity(cap) }
        }
        fn push(&mut self, v_ms: f32) {
            if self.data.len() == self.cap { self.data.pop_front(); }
            self.data.push_back(v_ms);
        }
        fn p(&self, q: f32) -> f32 {
            if self.data.is_empty() { return 0.0; }
            let mut v: Vec<f32> = self.data.iter().copied().collect();
            v.sort_by(|a,b| a.partial_cmp(b).unwrap());
            let idx = ((v.len() - 1) as f32 * q).round() as usize;
            v[idx]
        }
    }
    let mut tick_win = Win::new(240); //~4s at 60Hz
    let mut last_hb = Instant::now();

    #[derive(Default)]
    struct PhaseMs { sim:f32, snapshot:f32, grids:f32, serialize:f32, send:f32, loop_total:f32 }
    let mut ph = PhaseMs::default();
    let mut tick_i: u64 = 0;

    loop {
        let loop_start = Instant::now();
        
        let p0 = Instant::now();
        let mut crossings: Vec<(ClientId, CellKey, PlayerState)> = Vec::new();
        let mut sim_events: Vec<ServerEvent> = Vec::new();
        if last_sim.elapsed() >= SIM_DT {
            let dt = last_sim.elapsed().as_secs_f32().max(0.0001);
            last_sim = Instant::now();

            {
                let mut w = state.world.write().await;

                while let Ok(ctrl) = ctrl_rx.try_recv() {
                    match ctrl {
                        ControlMsg::Spawn(id) => {
                            let bytes = id.as_bytes();
                            let team = (bytes[0] ^ bytes[1] ^ bytes[2] ^ bytes[3]) & 1;
                            w.players.entry(id).or_insert(Player {
                                x: 0.0, y: 0.0, vx: 0.0, vy: 0.0, ang: 0.0, 
                                hp: 100,
                                max_hp: 100,
                                respawn_at: None, 
                                last_cell: (0,0),
                                transferring: false,
                                team,   
                            });
                            w.last_input.entry(id).or_default();
                            w.last_shoot.entry(id).or_insert(false);
                        }
                        ControlMsg::Despawn(id) => {
                            w.players.remove(&id);
                            w.last_input.remove(&id);
                            w.last_shoot.remove(&id);
                        }
                        ControlMsg::ControlSpawnAt { player_id, tile } => {
                            let world_pos = tile_to_world(tile);
                        }
                    }
                }

                while let Ok((id, inp)) = input_rx.try_recv() {
                    if let Some(s) = w.last_input.get_mut(&id) {
                        *s = InputState { up:inp.up, down:inp.down, left:inp.left, right:inp.right, shoot:inp.shoot };
                    }
                }

                let ids: Vec<ClientId> = w.players.keys().copied().collect();
                let mut pending_shots: Vec<(ClientId, f32, f32, f32, f32)> = Vec::new();

                for id in ids {
                    let s = w.last_input.get(&id).cloned().unwrap_or_default();
                    let prev_shoot = *w.last_shoot.get(&id).unwrap_or(&false);

                    if let Some(p) = w.players.get_mut(&id) {
                        let alive = p.hp > 0;
                        
                        let rot_dir = (s.right as i8 as f32) - (s.left as i8 as f32);
                        if alive {
                            p.ang += rot_dir * turn_speed * dt;
                            if p.ang >  std::f32::consts::PI { p.ang -= 2.0 * std::f32::consts::PI; }
                            if p.ang < -std::f32::consts::PI { p.ang += 2.0 * std::f32::consts::PI; }
                        }
                        
                        let fwd = if alive { (s.up as i8 as f32) - (s.down as i8 as f32) } else { 0.0 };
                        if fwd.abs() > 0.0 {
                            let dirx = p.ang.cos();
                            let diry = p.ang.sin();
                            p.vx = dirx * player_speed * fwd;
                            p.vy = diry * player_speed * fwd;
                        } else { p.vx = 0.0; p.vy = 0.0; }

                        if alive && s.shoot && !prev_shoot {
                            let dirx = p.ang.cos();
                            let diry = p.ang.sin();
                            let x = p.x + dirx * muzzle;
                            let y = p.y + diry * muzzle;
                            pending_shots.push((id, x, y, dirx, diry));
                        }
                    }

                    w.last_shoot.insert(id, s.shoot);
                }

                let ids_for_collision: Vec<ClientId> = w.players.keys().copied().collect();
                for id in ids_for_collision {
                    let (team, mut nx, mut ny) = match w.players.get_mut(&id) {
                        Some(p) => {
                            if p.hp <= 0 {
                                p.vx = 0.0; p.vy = 0.0;
                                continue;
                            }
                            (p.team, p.x + p.vx * dt, p.y + p.vy * dt)
                        }
                        None => continue,
                    };
                    
                    resolve_circle_vs_tiles(&mut w, &mut nx, &mut ny, PLAYER_RADIUS);

                    resolve_vs_enemy_players(id, team, &mut nx, &mut ny, &w.players);

                    if let Some(p) = w.players.get_mut(&id) {
                        p.x = nx;
                        p.y = ny;
                    }
                }
                for b in w.bullets.values_mut() {
                    b.x += b.vx * dt;
                    b.y += b.vy * dt;
                }

                {
                    let samples: Vec<(BulletId, f32, f32)> =
                        w.bullets.iter().map(|(bid, b)| (*bid, b.x, b.y)).collect();

                    let mut to_kill: Vec<BulletId> = Vec::new();
                    for (bid, bx, by) in samples {
                        let t = tile_at_world(&mut w, bx, by);
                        if t.blocks_bullet() {
                            to_kill.push(bid);
                        }
                    }
                    for bid in to_kill {
                        w.bullets.remove(&bid);
                    }
                }

                for (owner, x, y, dirx, diry) in pending_shots {
                    w.next_bullet_id += 1;
                    let bid = w.next_bullet_id;
                    w.bullets.insert(bid, Bullet {
                        id: bid, x, y, vx: dirx * bullet_speed, vy: diry * bullet_speed,
                        owner, born_ms: now_millis(),
                    });
                }

                w.bullets.retain(|_, b| b.x.abs() < 10000.0 && b.y.abs() < 10000.0);

                let nowms = now_millis();
                let mut dead_bullets: Vec<BulletId> = Vec::new();

                let bullets_for_hit: Vec<(BulletId, ClientId, f32, f32, u64)> = w.bullets
                    .iter()
                    .map(|(bid, b)| (*bid, b.owner, b.x, b.y, b.born_ms))
                    .collect();

                for (bid, owner, bx, by, born) in bullets_for_hit {
                    if nowms.saturating_sub(born) < grace_ms { continue; }
                    let owner_team = w.players.get(&owner).map(|p| p.team).unwrap_or(255);

                    for (pid, p) in w.players.iter_mut() {
                        if *pid == owner { continue; }
                        if p.team == owner_team { continue; } //friendly pass-through
                        if p.hp <= 0 { continue; }

                        let dx = bx - p.x; let dy = by - p.y;
                        if dx*dx + dy*dy <= hit_r2 {
                            p.hp -= BULLET_DMG;
                            let hp_now = p.hp;
                            sim_events.push(ServerEvent::Hit { src: owner, dst: *pid, dmg: BULLET_DMG, hp: hp_now });
                            if p.hp <= 0 && p.respawn_at.is_none() {
                                p.respawn_at = Some(nowms + RESPAWN_MS);
                                sim_events.push(ServerEvent::Death { id: *pid });
                            }
                            dead_bullets.push(bid);
                            break;
                        }
                    }
                }
                for bid in dead_bullets { w.bullets.remove(&bid); }

                for (pid, p) in w.players.iter_mut() {
                    if p.hp <= 0 {
                        if let Some(t) = p.respawn_at {
                            if nowms >= t {
                                let (sx, sy) = spawn_point_for(*pid);
                                p.x = sx; p.y = sy;
                                p.vx = 0.0; p.vy = 0.0;
                                p.hp = p.max_hp;
                                p.respawn_at = None;
                                sim_events.push(ServerEvent::Respawn { id: *pid, x: p.x, y: p.y });
                            }
                        }
                    }
                }

                for (id, p) in w.players.iter_mut() {
                    let ck = cell_of(p.x, p.y, CELL_W);
                    if ck != p.last_cell && !p.transferring {
                        let bytes = id.as_bytes();
                        let team = (bytes[0] ^ bytes[1] ^ bytes[2] ^ bytes[3]) & 1;
                        crossings.push((
                            *id,
                            ck,
                            PlayerState { id:*id, x:p.x, y:p.y, vx:p.vx, vy:p.vy, ang:p.ang, hp:p.hp, max_hp:p.max_hp, respawn_at:p.respawn_at, team }
                        ));
                        p.transferring = true;
                    } else {
                        p.last_cell = ck;
                        p.transferring = false;
                    }
                }
            }
        }

        event_buffer.extend(sim_events.drain(..));

        ph.sim = 1000.0 * p0.elapsed().as_secs_f32();

        if !crossings.is_empty() {
            for (pid, ck, pstate) in crossings.drain(..) {
                let owner = match route.owner_of(ck.0, ck.1).await {
                    Ok(s) => s,
                    Err(e) => { eprintln!("[{}] route owner err: {}", state.server_id, e); continue; }
                };
                if owner != state.server_id {
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
                    let _ = state.nats
                        .publish(format!("handoff.{}.req", owner),
                                 serde_json::to_vec(&req).unwrap().into())
                        .await;
                } else {
                    let mut w = state.world.write().await;
                    if let Some(p) = w.players.get_mut(&pid) { p.last_cell = ck; p.transferring = false; }
                }
            }
        }

        if last_net.elapsed() >= NET_DT {
            last_net = Instant::now();

            let p1 = Instant::now();
            let (players_snapshot, bullets_snapshot) = {
                let w = state.world.read().await;
                let players_snapshot: Vec<(ClientId, f32, f32, f32, i32)> =
                    w.players.iter().map(|(id,p)| (*id, p.x, p.y, p.ang, p.hp)).collect();
                let bullets_snapshot: Vec<Bullet> = w.bullets.values().cloned().collect();
                (players_snapshot, bullets_snapshot)
            };
            ph.snapshot = 1000.0 * p1.elapsed().as_secs_f32();

            let p3 = Instant::now();
            let players_json  = serde_json::to_string(&players_snapshot).unwrap();
            let bullets_json  = serde_json::to_string(&bullets_snapshot).unwrap();

            let events_to_send: Vec<ServerEvent> = std::mem::take(&mut event_buffer);
            let events_json = serde_json::to_string(&events_to_send).unwrap();

            let tms = now_millis();
            ph.serialize = 1000.0 * p3.elapsed().as_secs_f32();

            let p4 = Instant::now();
            let clients_snapshot = state.clients.read().await.clone();

            use std::collections::HashMap;
            let mut pmap: HashMap<ClientId, (f32,f32,f32,i32)> = HashMap::with_capacity(players_snapshot.len());
            for (pid, x, y, ang, hp) in &players_snapshot {
                pmap.insert(*pid, (*x, *y, *ang, *hp));
            }

            const VIEW_W: i32 = 26;
            const VIEW_H: i32 = 18;

            for (id, ch) in clients_snapshot {
                let (px, py) = if let Some((x,y,_,_)) = pmap.get(&id) {
                    (*x, *y)
                } else {
                    (0.0, 0.0)
                };
                
                let (ptx, pty) = world_to_tile(px, py);
                let ox = ptx - VIEW_W/2;
                let oy = pty - VIEW_H/2;

                let tiles_json = {
                    let mut w = state.world.write().await;
                    let window = collect_tile_window(&mut w, ox, oy, VIEW_W, VIEW_H);
                    serde_json::to_string(&window).unwrap()
                };

                let txt = format!(
                    r#"{{"v":1,"type":"state","t":{},"me":"{}","players":{},"bullets":{},"tiles":{}}}"#,
                    tms, id, players_json, bullets_json, tiles_json
                );

                let _ = ch.send(Message::Text(txt));
            }
            ph.send = 1000.0 * p4.elapsed().as_secs_f32();
            
            if tick_i % 60 == 0 {
                println!(
                    "[{}] ms sim={:.1} snap={:.1} ser={:.1} send={:.1} p50={:.1} p95={:.1}",
                    state.server_id, ph.sim, ph.snapshot, ph.serialize, ph.send,
                    tick_win.p(0.50), tick_win.p(0.95)
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

        if frame < SIM_DT { tokio::time::sleep(SIM_DT - frame).await; }
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
    Router::new().route("/healthz", get(|| async {"ok"}))
}