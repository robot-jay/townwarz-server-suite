use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Instant};

use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use axum::http::StatusCode; 
use clap::Parser;
use serde::{Serialize};
use serde_json::json;
use tokio::sync::RwLock;
use twz_shared::{Heartbeat, RouteCellResponse, ServerRegister};

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "0.0.0.0:7000")]
    bind: String,
}

#[derive(Clone, Default)]
struct AppState {
    inner: Arc<RwLock<Registry>>,
}

#[derive(Default)]
struct Registry {
    servers: HashMap<String, ServerInfo>,
}

#[derive(Clone, Serialize)]
struct ServerInfo {
    id: String,
    http_addr: String,
    ws_addr: String,
    capacity_hint: u32,
    #[serde(skip)]
    last_hb: Instant,
    #[serde(skip)]
    last_tick_p50: f32,
    #[serde(skip)]
    last_tick_p95: f32,
    #[serde(skip)]
    last_players: usize,
    #[serde(skip)]
    last_bullets: usize,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let state = AppState::default();

    let app = Router::new()
        .route("/register", post(register))
        .route("/heartbeat", post(heartbeat))
        .route("/route/cell/:cx/:cy", get(route_cell))
        .route("/server/:id", get(get_server)) // NEW
        .route("/health", get(|| async { "ok" }))
        .with_state(state);

    let addr: SocketAddr = args.bind.parse().unwrap();
    println!("Conductor listening on {}", addr);
    axum::serve(tokio::net::TcpListener::bind(addr).await.unwrap(), app)
        .await
        .unwrap();
}

async fn register(State(state): State<AppState>, Json(body): Json<ServerRegister>) -> impl IntoResponse {
    let mut reg = state.inner.write().await;
    let info = ServerInfo {
        id: body.server_id.clone(),
        http_addr: body.http_addr,
        ws_addr: body.ws_addr,
        capacity_hint: body.capacity_hint,
        last_hb: Instant::now(),
        last_tick_p50: 0.0,
        last_tick_p95: 0.0,
        last_players: 0,
        last_bullets: 0,
    };
    reg.servers.insert(body.server_id.clone(), info);
    println!("[conductor] register {}", body.server_id);
    Json(json!({"ok":true}))
}

async fn heartbeat(State(state): State<AppState>, Json(hb): Json<Heartbeat>) -> impl IntoResponse {
    let mut reg = state.inner.write().await;
    if let Some(s) = reg.servers.get_mut(&hb.server_id) {
        s.last_hb = Instant::now();
        s.last_tick_p50 = hb.tick_ms_p50;
        s.last_tick_p95 = hb.tick_ms_p95;
        s.last_players = hb.players;
        s.last_bullets = hb.bullets;
        println!(
            "[conductor] hb {} p50={:.2}ms p95={:.2}ms players={} bullets={}",
            hb.server_id, hb.tick_ms_p50, hb.tick_ms_p95, hb.players, hb.bullets
        );
        Json(json!({"ok":true}))
    } else {
        Json(json!({"ok":false,"err":"unknown_server"}))
    }
}

async fn route_cell(Path((cx, _cy)): Path<(i32, i32)>) -> impl IntoResponse {
    let owner = if cx < 0 { "A" } else { "B" };
    println!("[conductor] route cell ({},{}) -> {}", cx, 0, owner);
    Json(RouteCellResponse {
        owner_server_id: owner.to_string(),
    })
}

async fn get_server(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ServerInfo>, (StatusCode, Json<serde_json::Value>)> {
    let reg = state.inner.read().await;
    if let Some(s) = reg.servers.get(&id) {
        // clone so we can return an owned Json<ServerInfo>
        Ok(Json(s.clone()))
    } else {
        Err((StatusCode::NOT_FOUND, Json(serde_json::json!({"error":"not_found"}))))
    }
}