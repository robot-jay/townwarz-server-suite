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
use tokio_tungstenite::{
    connect_async, tungstenite::Message as TMsg, MaybeTlsStream, WebSocketStream,
};
use twz_shared::{game_catalog, GameIdentity};
use twz_shared::{
    validate_town_name, BackendHello, CreateTownRequest, CreateTownResponse, EndGameRequest,
    GameConnection, GameEndReason, GameEntry, PlayerState, RouteCellResponse, Town, SUBJ_GAME_END,
    SUBJ_GAME_TOUCH, SUBJ_TOWN_CREATE,
};

mod auth;
mod economy;
mod play;

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

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()?;
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

async fn catalog_handler() -> impl IntoResponse {
    // Public, read-only definitions are fetched by the SPA on a separate origin.
    (
        [(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        axum::Json(game_catalog()),
    )
}

async fn play_handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.max_message_size(32 * 1024)
        .on_upgrade(move |socket| play::session(state, socket))
}

async fn client_session(state: AppState, client_ws: WebSocket) {
    // Stable player id
    let pid = uuid::Uuid::new_v4().to_string();

    // Initial owner of the shared spawn cell.
    let owner = match route_owner(&state, 0, 0).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[gateway] route error: {e}");
            return;
        }
    };
    // Resolve backend ws addr
    let ws_addr = match get_server_ws(&state, &owner).await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[gateway] get server addr error: {e}");
            return;
        }
    };

    eprintln!("[gateway] ws_addr: {ws_addr}");

    // Track switches for this pid
    let (switch_tx, switch_rx) = mpsc::unbounded_channel::<String>();
    {
        state.switches.write().await.insert(pid.clone(), switch_tx);
    }

    // Connect backend #1 and split
    let url = format!("{}/ws", ws_addr.trim_end_matches('/'));
    match tokio::time::timeout(IO_TIMEOUT, connect_async(url)).await {
        Ok(Ok((backend, _))) => {
            relay_session(client_ws, backend, &pid, None, switch_rx, |owner| {
                let state = state.clone();
                async move { get_server_ws(&state, &owner).await }
            })
            .await;
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
    backend: BackendSocket,
    pid: &str,
    town: Option<&Town>,
    switch_rx: mpsc::UnboundedReceiver<String>,
    resolve_backend: F,
) where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<String>>,
{
    let _ = relay_with_context(
        client_ws,
        backend,
        pid,
        town,
        switch_rx,
        resolve_backend,
        None,
        None,
    )
    .await;
}

#[derive(Default)]
struct RelayOutcome {
    quit: bool,
    checkpoint: Option<PlayerState>,
}

#[derive(Clone)]
struct RelayLease {
    nats: async_nats::Client,
    connection: GameConnection,
    latest: Arc<RwLock<Option<PlayerState>>>,
}

async fn relay_with_context<F, Fut>(
    client_ws: WebSocket,
    mut backend: BackendSocket,
    pid: &str,
    town: Option<&Town>,
    mut switch_rx: mpsc::UnboundedReceiver<String>,
    resolve_backend: F,
    resume: Option<&PlayerState>,
    lease: Option<RelayLease>,
) -> RelayOutcome
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<String>>,
{
    let set_pid = TMsg::Text(
        serde_json::to_string(&BackendHello::SetPid {
            id: uuid::Uuid::parse_str(pid).expect("gateway-generated player id"),
            town: town.cloned(),
            identity: Some(match town {
                Some(town) => {
                    GameIdentity::mayor(uuid::Uuid::parse_str(pid).unwrap(), town.id.clone())
                }
                None => GameIdentity::debug(uuid::Uuid::parse_str(pid).unwrap()),
            }),
            resume_state: resume.cloned(),
        })
        .unwrap(),
    );
    if !matches!(
        tokio::time::timeout(IO_TIMEOUT, backend.send(set_pid.clone())).await,
        Ok(Ok(()))
    ) {
        return RelayOutcome {
            quit: false,
            checkpoint: resume.cloned(),
        };
    }
    let (mut c_sink, mut c_stream) = client_ws.split();
    let (mut b_sink, mut b_stream) = backend.split();
    let mut switches_open = true;
    let mut idle_check = tokio::time::interval(Duration::from_secs(10));
    let mut last_client_frame = std::time::Instant::now();
    let mut outcome = RelayOutcome {
        quit: false,
        checkpoint: resume.cloned(),
    };
    let mut economy = lease.as_ref().map(economy::SessionEconomy::new);
    let mut income_tick = tokio::time::interval(Duration::from_millis(500));
    let mut economic_cash = resume.map(|state| state.game.wallet.cash).unwrap_or(0);

    // Main relay loop
    loop {
        tokio::select! {
            _ = income_tick.tick(), if economy.is_some() => {
                if let Some(economy) = &economy { economy.request(None); }
            }
            response = async { economy.as_mut().unwrap().responses.recv().await }, if economy.is_some() => {
                if let Some(response) = response {
                    if let Some(projection) = response.projection {
                        economic_cash = projection.cash;
                        let message = twz_shared::ConstructionClientMessage::EconomyState {
                            cash: projection.cash, revision: projection.revision, owned_buildings: projection.buildings,
                            territory: twz_shared::TownTerritory::for_town(&projection.town),
                            constructible_types: twz_shared::CONSTRUCTIBLE_BUILDINGS.to_vec(), demolition_refund_percent: 25,
                        };
                        if c_sink.send(AxumMsg::Text(serde_json::to_string(&message).unwrap())).await.is_err() { break; }
                    }
                    if let Some(result) = response.result {
                        let message = twz_shared::ConstructionClientMessage::ConstructionResult {
                            result, outcome_known: response.error != Some(twz_shared::ConstructionError::ServiceUnavailable),
                        };
                        if c_sink.send(AxumMsg::Text(serde_json::to_string(&message).unwrap())).await.is_err() { break; }
                    }
                    if matches!(response.error, Some(twz_shared::ConstructionError::InactiveGame | twz_shared::ConstructionError::StaleConnection)) { break; }
                } else { break; }
            }
            // client -> backend
            message = c_stream.next() => {
                let Some(Ok(message)) = message else { break; };
                last_client_frame = std::time::Instant::now();
                if let AxumMsg::Text(text)=&message {
                    if let Ok(value)=serde_json::from_str::<serde_json::Value>(text) {
                        if value["type"]=="pause_game" {continue;}
                        if matches!(value["type"].as_str(), Some("place_building" | "demolish_building")) {
                            let mut value = value;
                            if let Some(object) = value.as_object_mut() { object.remove("v"); }
                            let request_id = value.get("request_id").cloned().unwrap_or(serde_json::Value::Null);
                            let command = serde_json::from_value::<twz_shared::ConstructionCommand>(value);
                            let error = match command {
                                Ok(command) => match &economy { Some(worker) if worker.request(Some(command.clone())) => None, Some(_) => Some(twz_shared::ConstructionError::Busy), None => Some(twz_shared::ConstructionError::InactiveGame) },
                                Err(_) => Some(twz_shared::ConstructionError::InvalidCommand),
                            };
                            if let Some(error) = error {
                                let _ = c_sink.send(AxumMsg::Text(serde_json::json!({"type":"construction_result","outcome_known":true,"result":{"request_id":request_id,"error":error,"building_id":null,"cash":economic_cash,"refund":0}}).to_string())).await;
                            }
                            continue;
                        }
                        if value["type"]=="quit_game" {
                            if let Some(lease)=&lease {
                                let request=EndGameRequest {player_id:lease.connection.player_id,connection_id:Some(lease.connection.connection_id),reason:GameEndReason::IntentionalQuit};
                                if play::operation(&lease.nats,SUBJ_GAME_END,&request).await.is_err() {
                                    let _=c_sink.send(AxumMsg::Text(serde_json::json!({"type":"game_error","message":"Could not confirm leaving the game. Please try again."}).to_string())).await;
                                    continue;
                                }
                            }
                            outcome.quit=true;
                            let _=tokio::time::timeout(IO_TIMEOUT,c_sink.send(AxumMsg::Text("{\"type\":\"game_quit_ack\"}".into()))).await;
                            break;
                        }
                    }
                }
                let Some(message) = axum_to_tungstenite(message) else { break; };
                if !matches!(tokio::time::timeout(IO_TIMEOUT, b_sink.send(message)).await, Ok(Ok(()))) { break; }
            }

            // backend -> client
            message = b_stream.next() => {
                let Some(Ok(message)) = message else { break; };
                let message = match message {
                    TMsg::Text(t) => {
                        if let Ok(value)=serde_json::from_str::<serde_json::Value>(&t) {
                            if let Some(snapshot)=value.get("resume_state").and_then(|v|serde_json::from_value::<PlayerState>(v.clone()).ok()) {
                                if snapshot.id.to_string()==pid {
                                    outcome.checkpoint=Some(snapshot.clone());
                                    if let Some(lease)=&lease {*lease.latest.write().await=Some(snapshot);}
                                }
                            }
                            if value["type"]=="game_ended" {
                                let _=c_sink.send(AxumMsg::Text(t)).await;
                                break;
                            }
                        }
                        AxumMsg::Text(t)
                    },
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
                    // Only the initial connection restores a checkpoint. A
                    // server handoff must attach to the destination's newer state.
                    let attach=BackendHello::SetPid {id:uuid::Uuid::parse_str(pid)?,town:town.cloned(),
                        identity:Some(match town {Some(t)=>GameIdentity::mayor(uuid::Uuid::parse_str(pid)?,t.id.clone()),None=>GameIdentity::debug(uuid::Uuid::parse_str(pid)?)}),resume_state:None};
                    socket.send(TMsg::Text(serde_json::to_string(&attach)?)).await?;
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

    // Capture eligible time at socket termination, before checkpoint/close IO.
    if let (Some(worker), Some(lease)) = (&economy, &lease) {
        let elapsed_ms = worker.elapsed_ms();
        worker.stop();
        if !outcome.quit {
            let _ = economy::request_economy(
                &lease.nats,
                &twz_shared::EconomyRequest {
                    player_id: lease.connection.player_id,
                    connection_id: lease.connection.connection_id,
                    elapsed_ms,
                    command: None,
                },
            )
            .await;
        }
    }
    if lease.is_some() && !outcome.quit {
        if matches!(
            tokio::time::timeout(
                IO_TIMEOUT,
                b_sink.send(TMsg::Text("{\"type\":\"pause_game\"}".into()))
            )
            .await,
            Ok(Ok(()))
        ) {
            let final_state = tokio::time::timeout(Duration::from_secs(2), async {
                while let Some(Ok(message)) = b_stream.next().await {
                    if let TMsg::Text(text) = message {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                            if value["type"] == "game_checkpoint" {
                                if let Ok(snapshot) =
                                    serde_json::from_value::<PlayerState>(value["state"].clone())
                                {
                                    if snapshot.id.to_string() == pid {
                                        return Some(snapshot);
                                    }
                                }
                            }
                        }
                    }
                }
                None
            })
            .await;
            if let Ok(Some(snapshot)) = final_state {
                outcome.checkpoint = Some(snapshot);
            }
        }
    }

    // Bound close writes so an unresponsive peer cannot keep a session alive.
    // Dropping both split halves afterwards also releases the TCP connections.
    let _ = tokio::join!(
        tokio::time::timeout(IO_TIMEOUT, b_sink.send(TMsg::Close(None))),
        tokio::time::timeout(IO_TIMEOUT, c_sink.send(AxumMsg::Close(None))),
    );
    outcome
}

/* ---------- helpers ---------- */

fn axum_to_tungstenite(msg: AxumMsg) -> Option<TMsg> {
    match msg {
        AxumMsg::Text(t) => Some(TMsg::Text(t)),
        AxumMsg::Binary(b) => Some(TMsg::Binary(b)),
        AxumMsg::Ping(p) => Some(TMsg::Ping(p)),
        AxumMsg::Pong(p) => Some(TMsg::Pong(p)),
        AxumMsg::Close(_) => None,
    }
}

async fn route_owner(state: &AppState, cx: i32, cy: i32) -> Result<String> {
    let r = state
        .http
        .get(format!("{}/route/cell/{}/{}", &state.conductor, cx, cy))
        .send()
        .await?
        .error_for_status()?;
    let body: RouteCellResponse = r.json().await?;
    Ok(body.owner_server_id)
}

#[derive(Deserialize)]
struct ServerInfo {
    ws_addr: String,
}

async fn get_server_ws(state: &AppState, id: &str) -> Result<String> {
    let r = state
        .http
        .get(format!("{}/server/{}", &state.conductor, id))
        .send()
        .await?
        .error_for_status()?;
    let info: ServerInfo = r.json().await?;
    Ok(info.ws_addr)
}

/* Global NATS listener: “switch.<pid>” -> forward to that session’s channel */
async fn global_switch_listener(state: AppState) {
    let mut sub = match state.nats.subscribe("switch.*".to_string()).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[gateway] switch sub err: {}", e);
            return;
        }
    };
    while let Some(msg) = sub.next().await {
        let subject = msg.subject.clone();
        let pid = subject.strip_prefix("switch.").unwrap_or("").to_string();
        if pid.is_empty() {
            continue;
        }
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
    Router::new().route("/healthz", get(|| async { "ok" }))
}

#[cfg(test)]
mod acceptance_tests;
#[cfg(test)]
mod tests;
