use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    hash::{Hash, Hasher},
    collections::hash_map::DefaultHasher,
    time::Duration
};
use tokio::{net::TcpStream, time::sleep, sync::RwLock};
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use axum::http::StatusCode;
use clap::Parser;
use serde::Serialize;
use serde_json::json;
use tracing_subscriber::EnvFilter;
use url::Url;

use twz_shared::{Heartbeat, RouteCellResponse, ServerRegister, CellKey};

mod town_registry;
use crate::town_registry::{TownRegistry, halls_from_map};

mod supervisor;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "0.0.0.0:7000")]
    bind: String,

    /// NATS url for async-nats
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats: String,
    
    #[arg(long, default_value = "false")]
    supervise: bool,

    #[arg(long, default_value = "conductor.toml")]
    config: std::path::PathBuf,
}

#[derive(Debug, Clone, Serialize)]
struct ServerInfo {
    server_id: String,
    http_addr: String,
    ws_addr: String,
    capacity_hint: u32,
    last_heartbeat_ms: u128,
}

#[derive(Default, Clone)]
struct Registry {
    servers: HashMap<String, ServerInfo>, // key: server_id
}

#[derive(Clone)]
struct AppState {
    inner: Arc<RwLock<Registry>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self { inner: Arc::new(RwLock::new(Registry::default())) }
    }

}

async fn wait_socket(host: &str, port: u16, max_secs: u64) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(max_secs);
    loop {
        match TcpStream::connect((host, port)).await {
            Ok(_) => return Ok(()),
            Err(e) => {
                if std::time::Instant::now() > deadline {
                    anyhow::bail!("timed out waiting for {}:{} ({e})", host, port);
                }
                tracing::debug!("waiting for {}:{} ... ({e})", host, port);
                sleep(Duration::from_millis(300)).await;
            }
        }
    }
}

async fn connect_nats_with_retry(url: &str, max_secs: u64) -> anyhow::Result<async_nats::Client> {
    let parsed = url::Url::parse(url)?;
    let host = parsed.host_str().unwrap_or("localhost").to_string();
    let port = parsed.port().unwrap_or(4222);
    // First ensure the socket is reachable (handles DNS + port-open)
    wait_socket(&host, port, max_secs).await?;
    // Then retry the actual NATS handshake a few times
    let start = std::time::Instant::now();
    loop {
        match async_nats::connect(url).await {
            Ok(c) => return Ok(c),
            Err(e) => {
                if std::time::Instant::now().duration_since(start).as_secs() > max_secs {
                    return Err(e.into());
                }
                tracing::debug!("retrying NATS connect to {} ... ({e})", url);
                sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .with_target(false)
        .init();

    let args = Args::parse();
    let state = AppState::default();

    if args.supervise {
        // start supervisor in the background
        let cfg_path = args.config.clone();
        tokio::spawn(async move {
            use crate::supervisor::{Supervisor, Backend};
            if let Ok(sup) = Supervisor::from_toml_file(Backend::Docker, cfg_path, Duration::from_secs(10)) {
                if let Err(e) = sup.run().await {
                    tracing::error!("supervisor error: {e:#}");
                }
            } else {
                tracing::error!("failed to load supervisor config");
            }
        });
    }

    // === Axum HTTP router ===
    let app = Router::new()
        .route("/register", post(register))
        .route("/heartbeat", post(heartbeat))
        .route("/route_cell/:id", get(route_cell))
        .route("/route/cell/:cx/:cy", get(route_cell_slash)) 
        .route("/server/:id", get(get_server))
        .with_state(state.clone());

    let addr: SocketAddr = args.bind.parse()?;
    tracing::info!("Conductor HTTP listening on {}", addr);

    // Serve HTTP
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("http server error: {e}");
        }
    });

    // === Async NATS + TownRegistry ===
    let nats_url = args.nats.clone();

    // If you’re in supervise mode and using the default service name, give DNS a moment
    if args.supervise && nats_url.contains("twz-nats") {
        // 30s is generous; tune as you like
        wait_socket("twz-nats", 4222, 30).await?;
    }

    let client = connect_nats_with_retry(&nats_url, 30).await?;
    let _js = async_nats::jetstream::new(client.clone());

    // TODO: replace with your real world tile source.
    // Format: Vec<(CellKey, Vec<(x, y, tileVal)>)>, where tileVal == 3 => Town Hall.
    let cells_with_tiles: Vec<(CellKey, Vec<(i32, i32, u8)>)> = vec![
        ((0, 0), vec![(10, 10, 3)]),
    ];

    let halls = halls_from_map(&cells_with_tiles);
    let reg = TownRegistry::new(client.clone(), halls).await?;
    reg.serve().await?;

    // keep alive
    std::future::pending::<()>().await;
    Ok(())
}

async fn register(
    State(state): State<AppState>,
    Json(body): Json<ServerRegister>,
) -> impl IntoResponse {
    let mut reg = state.inner.write().await;
    let now = now_ms();
    let info = ServerInfo {
        server_id: body.server_id.clone(),
        http_addr: body.http_addr.clone(),
        ws_addr: body.ws_addr.clone(),
        capacity_hint: body.capacity_hint,
        last_heartbeat_ms: now,
    };
    reg.servers.insert(body.server_id.clone(), info);
    (StatusCode::OK, Json(json!({"ok":true})))
}

async fn heartbeat(
    State(state): State<AppState>,
    Json(body): Json<Heartbeat>,
) -> impl IntoResponse {
    let mut reg = state.inner.write().await;
    if let Some(s) = reg.servers.get_mut(&body.server_id) {
        s.last_heartbeat_ms = now_ms();
        (StatusCode::OK, Json(json!({"ok":true})))
    } else {
        (StatusCode::NOT_FOUND, Json(json!({"ok":false,"error":"unknown_server"})))
    }
}

// e.g. GET /route_cell/12,34
async fn route_cell(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let parts: Vec<_> = id.split(',').collect();
    if parts.len() != 2 {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"bad_cell_id"})));
    }
    let cx: i32 = match parts[0].parse() { Ok(v) => v, Err(_) => return (StatusCode::BAD_REQUEST, Json(json!({"error":"bad_cell_x"}))) };
    let cy: i32 = match parts[1].parse() { Ok(v) => v, Err(_) => return (StatusCode::BAD_REQUEST, Json(json!({"error":"bad_cell_y"}))) };

    let reg = state.inner.read().await;
    if reg.servers.is_empty() {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":"no_servers"})));
    }

    // Consistent-ish hashing: map cell -> index of sorted server_ids
    let mut ids: Vec<&String> = reg.servers.keys().collect();
    ids.sort_unstable();

    let mut hasher = DefaultHasher::new();
    format!("{},{}", cx, cy).hash(&mut hasher);
    let idx = (hasher.finish() as usize) % ids.len();
    let owner_server_id = ids[idx].clone();

    let resp = RouteCellResponse { owner_server_id: owner_server_id.clone() };
    (StatusCode::OK, Json(serde_json::to_value(resp).unwrap()))
}

async fn route_cell_slash(
    State(state): State<AppState>,
    Path((cx, cy)): Path<(i32, i32)>,
) -> impl IntoResponse {
    // Reuse the same consistent hashing logic you already use
    let reg = state.inner.read().await;
    if reg.servers.is_empty() {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error":"no_servers"})));
    }

    let mut ids: Vec<&String> = reg.servers.keys().collect();
    ids.sort_unstable();

    let mut hasher = DefaultHasher::new();
    format!("{},{}", cx, cy).hash(&mut hasher);
    let idx = (hasher.finish() as usize) % ids.len();
    let owner_server_id = ids[idx].clone();

    let resp = RouteCellResponse { owner_server_id: owner_server_id.clone() };
    (StatusCode::OK, Json(serde_json::to_value(resp).unwrap()))
}

async fn get_server(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ServerInfo>, (StatusCode, Json<serde_json::Value>)> {
    let reg = state.inner.read().await;
    if let Some(s) = reg.servers.get(&id) {
        Ok(Json(s.clone()))
    } else {
        Err((StatusCode::NOT_FOUND, Json(serde_json::json!({"error":"not_found"}))))
    }
}

fn now_ms() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()
}