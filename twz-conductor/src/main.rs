use axum::http::StatusCode;
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use serde::Serialize;
use serde_json::json;
use std::{
    collections::hash_map::DefaultHasher,
    collections::HashMap,
    hash::{Hash, Hasher},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpStream, sync::RwLock, time::sleep};
use tracing_subscriber::EnvFilter;

use twz_shared::{Heartbeat, RouteCellResponse, ServerRegister};

mod economy;
mod persistence;
mod town_registry;
use crate::town_registry::TownRegistry;

mod supervisor;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "0.0.0.0:7000")]
    bind: String,

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
    owners: HashMap<twz_shared::CellKey, String>,
    routing_ready: bool,
}

#[derive(Clone)]
struct AppState {
    inner: Arc<RwLock<Registry>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Registry::default())),
        }
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

    wait_socket(&host, port, max_secs).await?;

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
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .with_target(false)
        .init();

    let args = Args::parse();
    let state = AppState::default();

    if args.supervise {
        let cfg_path = args.config.clone();
        tokio::spawn(async move {
            use crate::supervisor::{Backend, Supervisor};
            if let Ok(sup) =
                Supervisor::from_toml_file(Backend::Docker, cfg_path, Duration::from_secs(10))
            {
                if let Err(e) = sup.run().await {
                    tracing::error!("supervisor error: {e:#}");
                }
            } else {
                tracing::error!("failed to load supervisor config");
            }
        });
    }

    let app = Router::new()
        .route("/register", post(register))
        .route("/heartbeat", post(heartbeat))
        .route("/route_cell/:id", get(route_cell))
        .route("/route/cell/:cx/:cy", get(route_cell_slash))
        .route("/server/:id", get(get_server))
        .with_state(state.clone());

    let addr: SocketAddr = args.bind.parse()?;
    tracing::info!("Conductor HTTP listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("http server error: {e}");
        }
    });

    let nats_url = args.nats.clone();

    if args.supervise && nats_url.contains("twz-nats") {
        wait_socket("twz-nats", 4222, 30).await?;
    }

    let client = connect_nats_with_retry(&nats_url, 30).await?;
    let database_url = std::env::var("DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("Set DATABASE_URL in the server suite .env file"))?;
    let reg = TownRegistry::new(client.clone(), &database_url).await?;
    reg.serve(state).await?;

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
        (
            StatusCode::NOT_FOUND,
            Json(json!({"ok":false,"error":"unknown_server"})),
        )
    }
}

async fn route_cell(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let parts: Vec<_> = id.split(',').collect();
    if parts.len() != 2 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"bad_cell_id"})),
        );
    }
    let cx: i32 = match parts[0].parse() {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(json!({"error":"bad_cell_x"}))),
    };
    let cy: i32 = match parts[1].parse() {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(json!({"error":"bad_cell_y"}))),
    };

    route_response(&state, (cx, cy)).await
}

async fn route_cell_slash(
    State(state): State<AppState>,
    Path((cx, cy)): Path<(i32, i32)>,
) -> impl IntoResponse {
    route_response(&state, (cx, cy)).await
}

async fn route_response(
    state: &AppState,
    cell: twz_shared::CellKey,
) -> (StatusCode, Json<serde_json::Value>) {
    match owner_for_cell(state, cell).await {
        Some(owner_server_id) => (
            StatusCode::OK,
            Json(serde_json::to_value(RouteCellResponse { owner_server_id }).unwrap()),
        ),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"authority_unavailable"})),
        ),
    }
}

async fn owner_for_cell(state: &AppState, cell: twz_shared::CellKey) -> Option<String> {
    let mut reg = state.inner.write().await;
    if !reg.routing_ready {
        return None;
    }
    if let Some(owner) = reg.owners.get(&cell) {
        return reg.servers.contains_key(owner).then(|| owner.clone());
    }
    let mut ids: Vec<_> = reg.servers.keys().cloned().collect();
    ids.sort_unstable();
    if ids.is_empty() {
        return None;
    }
    let mut hash = DefaultHasher::new();
    format!("{},{}", cell.0, cell.1).hash(&mut hash);
    let owner = ids[hash.finish() as usize % ids.len()].clone();
    reg.owners.insert(cell, owner.clone());
    Some(owner)
}

async fn get_server(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ServerInfo>, (StatusCode, Json<serde_json::Value>)> {
    let reg = state.inner.read().await;
    if let Some(s) = reg.servers.get(&id) {
        Ok(Json(s.clone()))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error":"not_found"})),
        ))
    }
}

fn now_ms() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

#[cfg(test)]
mod routing_tests {
    use super::*;
    #[tokio::test]
    async fn registrations_do_not_reassign_known_cells_and_missing_pinned_owners_fail_closed() {
        let state = AppState::default();
        state.inner.write().await.routing_ready = true;
        let info = |id: &str| ServerInfo {
            server_id: id.into(),
            http_addr: String::new(),
            ws_addr: String::new(),
            capacity_hint: 1,
            last_heartbeat_ms: 0,
        };
        state
            .inner
            .write()
            .await
            .servers
            .insert("A".into(), info("A"));
        assert_eq!(owner_for_cell(&state, (0, 0)).await.as_deref(), Some("A"));
        state
            .inner
            .write()
            .await
            .servers
            .insert("B".into(), info("B"));
        assert_eq!(owner_for_cell(&state, (0, 0)).await.as_deref(), Some("A"));
        state.inner.write().await.servers.remove("A");
        assert!(owner_for_cell(&state, (0, 0)).await.is_none());
        assert_eq!(owner_for_cell(&state, (1, 0)).await.as_deref(), Some("B"));
    }
}
