use std::{collections::HashMap, future::Future, net::SocketAddr, sync::Arc, time::Duration};

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
use tokio_tungstenite::{connect_async, tungstenite::Message as TMsg, MaybeTlsStream, WebSocketStream};
use twz_shared::{RouteCellResponse, BackendHello, Town, GameEntry, CreateTownRequest, CreateTownResponse, GameSession, validate_town_name, SUBJ_TOWN_CREATE, SUBJ_GAME_END, SUBJ_GAME_TOUCH};
use twz_shared::{GameIdentity, game_catalog};

mod auth;

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
    auth: Option<auth::Auth0>,
    // player_id -> switch channel
    switches: Arc<RwLock<HashMap<String, mpsc::UnboundedSender<String>>>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args = Args::parse();

    let http = reqwest::Client::builder().timeout(Duration::from_secs(8)).build()?;
    let nats = async_nats::connect(args.nats.clone()).await?;

    let state = AppState {
        conductor: args.conductor.clone(),
        http,
        nats: nats.clone(),
        auth: auth::Auth0::from_env(),
        switches: Arc::new(RwLock::new(HashMap::new())),
    };

    // subscribe to switch.* and fan out to sessions
    tokio::spawn(global_switch_listener(state.clone()));

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ws", get(ws_handler))
        .route("/play", get(play_handler))
        .route("/catalog", get(catalog_handler))
        .with_state(state.clone());

    let addr: SocketAddr = args.bind.parse().unwrap();
    println!("[gateway] listening on {}", addr);
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}

async fn ws_handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |client_ws| client_session(state, client_ws))
}

async fn catalog_handler() -> axum::Json<twz_shared::GameCatalog> {
    axum::Json(game_catalog())
}

async fn play_handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.max_message_size(32 * 1024).on_upgrade(move |socket| play_session(state, socket))
}

async fn play_session(state: AppState, mut client: WebSocket) {
    let pid = uuid::Uuid::new_v4();
    let result = tokio::time::timeout(Duration::from_secs(25), async {
        let Some(Ok(AxumMsg::Text(text))) = client.next().await else {
            anyhow::bail!("Game setup was interrupted. Please start a new game.");
        };
        let GameEntry::StartGame { id_token, role, town_name } = serde_json::from_str(&text)
            .map_err(|_| anyhow::anyhow!("Expected a new-game request."))?;
        anyhow::ensure!(role == "mayor", "Commando is not available yet. Please choose mayor.");
        let desired_name = validate_town_name(&town_name).map_err(anyhow::Error::msg)?;
        let auth = state.auth.as_ref().ok_or_else(|| anyhow::anyhow!("The gateway needs AUTH0_DOMAIN and AUTH0_CLIENT_ID configured."))?;
        let user = auth.verify(&state.http, &id_token).await
            .map_err(|_| anyhow::anyhow!("Your sign-in could not be verified. Sign out and sign in again."))?;
        let request = CreateTownRequest { player_id: pid, desired_name, user };
        let reply = state.nats.request(SUBJ_TOWN_CREATE, serde_json::to_vec(&request)?.into()).await
            .map_err(|_| anyhow::anyhow!("The town service is unavailable. Please try again."))?;
        let response: CreateTownResponse = serde_json::from_slice(&reply.payload)?;
        anyhow::ensure!(response.ok, "{}", response.error.unwrap_or_else(|| "Unable to create town.".into()));
        let town = response.town.ok_or_else(|| anyhow::anyhow!("Missing town allocation."))?;
        let owner = route_owner(&state, town.hall.cell.0, town.hall.cell.1).await
            .map_err(|_| anyhow::anyhow!("No cell server is available. Please try again."))?;
        let address = get_server_ws(&state, &owner).await?;
        let (backend, _) = tokio::time::timeout(IO_TIMEOUT, connect_async(format!("{}/ws", address.trim_end_matches('/')))).await??;
        Ok::<_, anyhow::Error>((town, backend))
    }).await;

    if let Ok(Ok((town, backend))) = result {
        let pid_text = pid.to_string();
        let (switch_tx, switch_rx) = mpsc::unbounded_channel();
        state.switches.write().await.insert(pid.to_string(), switch_tx);
        let relay = relay_session(client, backend, &pid_text, Some(&town), switch_rx, |owner| {
            let state = state.clone();
            async move { get_server_ws(&state, &owner).await }
        });
        tokio::pin!(relay);
        let mut lease = tokio::time::interval(Duration::from_secs(10));
        loop {
            tokio::select! {
                _ = &mut relay => break,
                _ = lease.tick() => {
                    let _ = state.nats.publish(SUBJ_GAME_TOUCH, serde_json::to_vec(&GameSession { player_id: pid }).unwrap().into()).await;
                }
            }
        }
        state.switches.write().await.remove(&pid.to_string());
    } else {
        let message = match result {
            Ok(Err(error)) => error.to_string(),
            _ => "Game setup timed out. Please try again.".into(),
        };
        let _ = tokio::time::timeout(IO_TIMEOUT, client.send(AxumMsg::Text(serde_json::json!({"v":1,"type":"game_error","message":message}).to_string()))).await;
        let _ = tokio::time::timeout(IO_TIMEOUT, client.send(AxumMsg::Close(None))).await;
    }
    // Also end allocations when setup failed after the database transaction.
    let _ = state.nats.publish(SUBJ_GAME_END, serde_json::to_vec(&GameSession { player_id: pid }).unwrap().into()).await;
}

async fn client_session(state: AppState, client_ws: WebSocket) {
    // Stable player id
    let pid = uuid::Uuid::new_v4().to_string();

    // Initial owner of the shared spawn cell.
    let owner = match route_owner(&state, 0, 0).await {
        Ok(s) => s, Err(e) => { eprintln!("[gateway] route error: {e}"); return; }
    };
    // Resolve backend ws addr
    let ws_addr = match get_server_ws(&state, &owner).await {
        Ok(a) => a, Err(e) => { eprintln!("[gateway] get server addr error: {e}"); return; }
    };

    eprintln!("[gateway] ws_addr: {ws_addr}");

    // Track switches for this pid
    let (switch_tx, switch_rx) = mpsc::unbounded_channel::<String>();
    { state.switches.write().await.insert(pid.clone(), switch_tx); }

    // Connect backend #1 and split
    let url = format!("{}/ws", ws_addr.trim_end_matches('/'));
    match tokio::time::timeout(IO_TIMEOUT, connect_async(url)).await {
        Ok(Ok((backend, _))) => {
            relay_session(client_ws, backend, &pid, None, switch_rx, |owner| {
                let state = state.clone();
                async move { get_server_ws(&state, &owner).await }
            }).await;
        }
        Ok(Err(error)) => eprintln!("[gateway] backend connect failed: {error}"),
        Err(_) => eprintln!("[gateway] backend connect timed out"),
    }
    state.switches.write().await.remove(&pid);
}

const IO_TIMEOUT: Duration = Duration::from_secs(5);
type BackendSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

// Read both sockets in this task: EOF/errors must end the whole session, not
// just a detached reader while the cell server keeps an orphan player alive.
async fn relay_session<F, Fut>(
    client_ws: WebSocket,
    mut backend: BackendSocket,
    pid: &str,
    town: Option<&Town>,
    mut switch_rx: mpsc::UnboundedReceiver<String>,
    resolve_backend: F,
) where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<String>>,
{
    let set_pid = TMsg::Text(serde_json::to_string(&BackendHello::SetPid {
        id: uuid::Uuid::parse_str(pid).expect("gateway-generated player id"), town: town.cloned(),
        identity: Some(match town {
            Some(town) => GameIdentity::mayor(uuid::Uuid::parse_str(pid).unwrap(), town.id.clone()),
            None => GameIdentity::debug(uuid::Uuid::parse_str(pid).unwrap()),
        }),
    }).unwrap());
    if !matches!(tokio::time::timeout(IO_TIMEOUT, backend.send(set_pid.clone())).await, Ok(Ok(()))) {
        return;
    }
    let (mut c_sink, mut c_stream) = client_ws.split();
    let (mut b_sink, mut b_stream) = backend.split();
    let mut switches_open = true;
    let mut idle_check = tokio::time::interval(Duration::from_secs(10));
    let mut last_client_frame = std::time::Instant::now();

    // Main relay loop
    loop {
        tokio::select! {
            // client -> backend
            message = c_stream.next() => {
                let Some(Ok(message)) = message else { break; };
                last_client_frame = std::time::Instant::now();
                let Some(message) = axum_to_tungstenite(message) else { break; };
                if !matches!(tokio::time::timeout(IO_TIMEOUT, b_sink.send(message)).await, Ok(Ok(()))) { break; }
            }

            // backend -> client
            message = b_stream.next() => {
                let Some(Ok(message)) = message else { break; };
                let message = match message {
                    TMsg::Text(t) => AxumMsg::Text(t),
                    TMsg::Binary(b) => AxumMsg::Binary(b),
                    TMsg::Close(_) => break,
                    TMsg::Ping(p) => AxumMsg::Ping(p),
                    TMsg::Pong(p) => AxumMsg::Pong(p),
                    _ => continue,
                };
                if !matches!(tokio::time::timeout(IO_TIMEOUT, c_sink.send(message)).await, Ok(Ok(()))) { break; }
            }

            // backend switch signal for this pid
            owner = switch_rx.recv(), if switches_open => {
                let Some(owner) = owner else { switches_open = false; continue; };
                let replacement = tokio::time::timeout(IO_TIMEOUT, async {
                    let address = resolve_backend(owner.clone()).await?;
                    let (mut socket, _) = connect_async(format!("{}/ws", address.trim_end_matches('/'))).await?;
                    socket.send(set_pid.clone()).await?;
                    Ok::<_, anyhow::Error>(socket)
                }).await;
                match replacement {
                    Ok(Ok(socket)) => {
                        let (new_sink, new_stream) = socket.split();
                        // Explicitly close the previous cell connection on handoff.
                        let _ = tokio::time::timeout(IO_TIMEOUT, b_sink.send(TMsg::Close(None))).await;
                        b_sink = new_sink;
                        b_stream = new_stream;
                        println!("[gateway] switched backend -> {}", owner);
                    }
                    _ => eprintln!("[gateway] failed to connect new backend {}", owner),
                }
            }
            _ = idle_check.tick() => {
                if last_client_frame.elapsed() > Duration::from_secs(30) { break; }
            }
        }
    }

    // Bound close writes so an unresponsive peer cannot keep a session alive.
    // Dropping both split halves afterwards also releases the TCP connections.
    let _ = tokio::join!(
        tokio::time::timeout(IO_TIMEOUT, b_sink.send(TMsg::Close(None))),
        tokio::time::timeout(IO_TIMEOUT, c_sink.send(AxumMsg::Close(None))),
    );
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

pub fn health_router() -> Router {
    Router::new().route("/healthz", get(|| async {"ok"}))
}

#[cfg(test)]
mod tests;
