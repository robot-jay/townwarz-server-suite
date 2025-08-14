use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use anyhow::Result;
use axum::{
    extract::ws::{Message as AxumMsg, WebSocket, WebSocketUpgrade},
    extract::State,
    response::IntoResponse,
    routing::get,
    Router,
};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message as TMsg};
use twz_shared::RouteCellResponse;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "0.0.0.0:8081")]
    bind: String,

    #[arg(long, default_value = "http://127.0.0.1:7000")]
    conductor: String,

    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats: String,
}

#[derive(Clone)]
struct AppState {
    conductor: String,
    http: reqwest::Client,
    nats: async_nats::Client,
    // player_id -> switch channel
    switches: Arc<RwLock<HashMap<String, mpsc::UnboundedSender<String>>>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let http = reqwest::Client::new();
    let nats = async_nats::connect(args.nats.clone()).await?;

    let state = AppState {
        conductor: args.conductor.clone(),
        http,
        nats: nats.clone(),
        switches: Arc::new(RwLock::new(HashMap::new())),
    };

    // subscribe to switch.* and fan out to sessions
    tokio::spawn(global_switch_listener(state.clone()));

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ws", get(ws_handler))
        .with_state(state.clone());

    let addr: SocketAddr = args.bind.parse().unwrap();
    println!("[gateway] listening on {}", addr);
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}

async fn ws_handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |client_ws| client_session(state, client_ws))
}

async fn client_session(state: AppState, client_ws: WebSocket) {
    use futures_util::StreamExt; // ensure in scope
    use futures_util::SinkExt;

    // Stable player id
    let pid = uuid::Uuid::new_v4().to_string();

    // Initial owner (cx=0 => "B")
    let owner = match route_owner(&state, 0, 0).await {
        Ok(s) => s, Err(e) => { eprintln!("[gateway] route error: {e}"); return; }
    };
    // Resolve backend ws addr
    let ws_addr = match get_server_ws(&state, &owner).await {
        Ok(a) => a, Err(e) => { eprintln!("[gateway] get server addr error: {e}"); return; }
    };

    // Track switches for this pid
    let (switch_tx, mut switch_rx) = mpsc::unbounded_channel::<String>();
    { state.switches.write().await.insert(pid.clone(), switch_tx); }

    // --- Split the client WS into sink/stream so we can write & read concurrently
    let (mut c_sink, mut c_stream) = client_ws.split();

    // Connect backend #1 and split
    let url = format!("{}/ws", ws_addr.trim_end_matches('/'));
    let (ws, _resp) = match connect_async(url).await {
        Ok(ok) => ok,
        Err(e) => { eprintln!("[gateway] backend connect error: {e}"); state.switches.write().await.remove(&pid); return; }
    };
    let (mut b_sink, mut b_stream) = ws.split();

    // Tell backend our pid
    let _ = b_sink.send(TMsg::Text(format!(r#"{{"type":"set_pid","id":"{}"}}"#, pid))).await;

    // Channel for client->backend frames (survives backend swaps)
    let (tx_to_backend, mut rx_to_backend) = mpsc::unbounded_channel::<AxumMsg>();

    // Spawn: read client -> channel
    let tx_to_backend_clone = tx_to_backend.clone();
    tokio::spawn(async move {
        while let Some(Ok(msg)) = c_stream.next().await {
            // forward every client frame into the channel
            if tx_to_backend_clone.send(msg).is_err() { break; }
        }
    });

    // Main relay loop
    loop {
        tokio::select! {
            // client -> backend
            Some(msg) = rx_to_backend.recv() => {
                if let Some(tm) = axum_to_tungstenite(msg) {
                    if b_sink.send(tm).await.is_err() { break; }
                } else {
                    let _ = b_sink.send(TMsg::Close(None)).await;
                    break;
                }
            }

            // backend -> client
            Some(Ok(msg)) = b_stream.next() => {
                match msg {
                    TMsg::Text(t) => {
                        //println!("[gateway] <- backend TEXT: {}", t.chars().take(200).collect::<String>());
                        let _ = c_sink.send(AxumMsg::Text(t)).await;
                    }
                    TMsg::Binary(b) => {
                        //println!("[gateway] <- backend BIN {} bytes", b.len());
                        let _ = c_sink.send(AxumMsg::Binary(b)).await;
                    }
                    TMsg::Close(_) => { let _ = c_sink.send(AxumMsg::Close(None)).await; break; }
                    TMsg::Ping(p) => { let _ = c_sink.send(AxumMsg::Ping(p)).await; }
                    TMsg::Pong(p) => { let _ = c_sink.send(AxumMsg::Pong(p)).await; }
                    _ => {}
                }
            }

            // backend switch signal for this pid
            Some(new_owner) = switch_rx.recv() => {
                if let Ok(new_ws_addr) = get_server_ws(&state, &new_owner).await {
                    let url2 = format!("{}/ws", new_ws_addr.trim_end_matches('/'));
                    if let Ok((ws2, _r2)) = connect_async(url2).await {
                        let (mut new_sink, new_stream) = ws2.split();
                        // adopt pid on new backend
                        let _ = new_sink.send(TMsg::Text(format!(r#"{{"type":"set_pid","id":"{}"}}"#, pid))).await;
                        // swap
                        b_sink = new_sink;
                        b_stream = new_stream;
                        println!("[gateway] switched backend -> {}", new_owner);
                    } else {
                        eprintln!("[gateway] failed to connect new backend {}", new_owner);
                    }
                }
            }

            else => break,
        }
    }

    state.switches.write().await.remove(&pid);
}

/* ---------- helpers ---------- */

fn axum_to_tungstenite(msg: AxumMsg) -> Option<TMsg> {
    match msg {
        AxumMsg::Text(t)   => Some(TMsg::Text(t)),
        AxumMsg::Binary(b) => Some(TMsg::Binary(b)),
        AxumMsg::Ping(p)   => Some(TMsg::Ping(p)),
        AxumMsg::Pong(p)   => Some(TMsg::Pong(p)),
        AxumMsg::Close(_)  => None,
    }
}

async fn route_owner(state: &AppState, cx: i32, cy: i32) -> Result<String> {
    let r = state.http
        .get(format!("{}/route/cell/{}/{}", &state.conductor, cx, cy))
        .send().await?
        .error_for_status()?;
    let body: RouteCellResponse = r.json().await?;
    Ok(body.owner_server_id)
}

#[derive(Deserialize)]
struct ServerInfo { ws_addr: String }

async fn get_server_ws(state: &AppState, id: &str) -> Result<String> {
    let r = state.http
        .get(format!("{}/server/{}", &state.conductor, id))
        .send().await?
        .error_for_status()?;
    let info: ServerInfo = r.json().await?;
    Ok(info.ws_addr)
}

/* Global NATS listener: “switch.<pid>” -> forward to that session’s channel */
async fn global_switch_listener(state: AppState) {
    let mut sub = match state.nats.subscribe("switch.*".to_string()).await {
        Ok(s) => s, Err(e) => { eprintln!("[gateway] switch sub err: {}", e); return; }
    };
    while let Some(msg) = sub.next().await {
        let subject = msg.subject.clone();
        let pid = subject.strip_prefix("switch.").unwrap_or("").to_string();
        if pid.is_empty() { continue; }
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&msg.payload) {
            if let Some(owner) = v.get("to_server").and_then(|s| s.as_str()) {
                if let Some(tx) = state.switches.read().await.get(&pid).cloned() {
                    let _ = tx.send(owner.to_string());
                }
            }
        }
    }
}