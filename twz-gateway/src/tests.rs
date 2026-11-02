use super::*;
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle, time::timeout};

const TEST_TIMEOUT: Duration = Duration::from_secs(3);

struct Backend {
    url: String,
    connected: oneshot::Receiver<String>,
    disconnected: oneshot::Receiver<()>,
    task: JoinHandle<()>,
    identity: oneshot::Receiver<serde_json::Value>,
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn backend() -> Backend {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let (connected_tx, connected) = oneshot::channel();
    let (disconnected_tx, disconnected) = oneshot::channel();
    let (identity_tx, identity) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let first = ws.next().await.unwrap().unwrap().into_text().unwrap();
        let message: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(message["type"], "set_pid");
        let _ = identity_tx.send(message.clone());
        connected_tx.send(message["id"].as_str().unwrap().to_owned()).unwrap();
        if ws.send(TMsg::Text("ready".into())).await.is_err() {
            let _ = disconnected_tx.send(());
            return;
        }
        while let Some(Ok(message)) = ws.next().await {
            if message.is_close() {
                break;
            }
        }
        let _ = disconnected_tx.send(());
    });
    Backend { url, connected, disconnected, task, identity }
}

#[tokio::test]
#[ignore = "requires an isolated TEST_NATS_URL"]
async fn authenticated_entry_verifies_user_creates_named_town_and_ends_on_disconnect() -> Result<()> {
    let nats = async_nats::connect(std::env::var("TEST_NATS_URL")?).await?;
    let mut creates = nats.subscribe(SUBJ_TOWN_CREATE).await?;
    let mut ends = nats.subscribe(SUBJ_GAME_END).await?;
    nats.flush().await?;
    let mut backend = backend().await;
    let address = backend.url.clone();
    let conductor = TcpListener::bind("127.0.0.1:0").await?;
    let conductor_url = format!("http://{}", conductor.local_addr()?);
    let conductor_app = Router::new()
        .route("/route/cell/:cx/:cy", get(|| async { axum::Json(serde_json::json!({"owner_server_id":"test-cell"})) }))
        .route("/server/:id", get(move || {
            let address = address.clone();
            async move { axum::Json(serde_json::json!({"ws_addr":address})) }
        }));
    let conductor_task = tokio::spawn(async move { axum::serve(conductor, conductor_app).await.unwrap() });
    let (auth, tokens) = auth::Auth0::fixture();
    let state = AppState {
        conductor: conductor_url, http: reqwest::Client::new(), nats: nats.clone(), auth: Some(auth),
        switches: Arc::new(RwLock::new(HashMap::new())),
    };
    let app = Router::new().route("/play", get(play_handler)).with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("ws://{}/play", listener.local_addr()?);
    let gateway_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    for (role, token, name) in [("commando", "invalid", "Test Town"), ("mayor", "invalid", "Test Town"), ("mayor", "invalid", "<bad>")] {
        let (mut client, _) = connect_async(&url).await?;
        client.send(TMsg::Text(serde_json::json!({"type":"start_game","role":role,"id_token":token,"town_name":name}).to_string())).await?;
        let frame = timeout(TEST_TIMEOUT, client.next()).await?.unwrap()?;
        let response: serde_json::Value = serde_json::from_str(&frame.into_text()?)?;
        assert_eq!(response["type"], "game_error");
    }
    // Invalid entry requests never reach persistence or the cell connection.
    assert!(timeout(Duration::from_millis(100), creates.next()).await.is_err());

    let town_service = tokio::spawn(async move {
        let message = timeout(TEST_TIMEOUT, creates.next()).await.unwrap().unwrap();
        let request: CreateTownRequest = serde_json::from_slice(&message.payload).unwrap();
        assert_eq!(request.user.sub, "auth0|test-mayor");
        assert_eq!(request.desired_name, "Copper Creek");
        let town = Town {
            id: "test-town".into(), name: request.desired_name, hall: twz_shared::hall_for_slot(0),
            created_at_ms: 1, mayor_player_id: request.player_id,
        };
        let response = CreateTownResponse { ok:true, error:None, spawn_tile:Some(twz_shared::spawn_from_hall(town.hall.origin)), town:Some(town) };
        nats.publish(message.reply.unwrap(), serde_json::to_vec(&response).unwrap().into()).await.unwrap();
        request.player_id
    });
    let (mut client, _) = connect_async(&url).await?;
    client.send(TMsg::Text(serde_json::json!({"type":"start_game","role":"mayor","id_token":tokens["valid"],"town_name":"  Copper Creek  "}).to_string())).await?;
    let first = timeout(TEST_TIMEOUT, client.next()).await?.unwrap()?;
    assert_eq!(first.into_text()?, "ready");
    let identity = timeout(TEST_TIMEOUT, &mut backend.identity).await??;
    assert_eq!(identity["town"]["name"], "Copper Creek");
    let pid = town_service.await?;
    assert_eq!(identity["id"], pid.to_string());
    assert_eq!(identity["identity"]["game_id"], pid.to_string());
    assert_eq!(identity["identity"]["town_id"], "test-town");
    assert_eq!(identity["identity"]["role"], "mayor");
    drop(client);
    timeout(TEST_TIMEOUT, &mut backend.disconnected).await??;
    loop {
        let message = timeout(TEST_TIMEOUT, ends.next()).await?.unwrap();
        let ended: GameSession = serde_json::from_slice(&message.payload)?;
        if ended.player_id == pid { break; }
    }
    assert!(state.switches.read().await.is_empty());
    conductor_task.abort();
    gateway_task.abort();
    Ok(())
}

#[tokio::test]
#[ignore = "requires isolated TEST_NATS_URL and TEST_CONDUCTOR_URL with running PostgreSQL conductor/cells"]
async fn authenticated_entry_live_cells_spawn_label_and_start_fresh() -> Result<()> {
    async fn state_for(client: &mut BackendSocket, name: &str) -> Result<serde_json::Value> {
        Ok(timeout(Duration::from_secs(6), async {
            loop {
                let frame = client.next().await.ok_or_else(|| anyhow::anyhow!("game disconnected"))??;
                if let TMsg::Text(text) = frame {
                    let state: serde_json::Value = serde_json::from_str(&text)?;
                    if state["type"] == "game_error" { anyhow::bail!("{}", state["message"]); }
                    if state["type"] == "state" && state["towns"].as_array().is_some_and(|towns| towns.iter().any(|town| town["name"] == name)) {
                        return Ok::<_, anyhow::Error>(state);
                    }
                }
            }
        }).await??)
    }
    async fn enter(url: &str, token: &serde_json::Value, name: &str) -> Result<BackendSocket> {
        let (mut client, _) = connect_async(url).await?;
        client.send(TMsg::Text(serde_json::json!({"type":"start_game","role":"mayor","id_token":token,"town_name":name}).to_string())).await?;
        Ok(client)
    }
    let nats = async_nats::connect(std::env::var("TEST_NATS_URL")?).await?;
    let (auth, tokens) = auth::Auth0::fixture();
    let state = AppState {
        conductor: std::env::var("TEST_CONDUCTOR_URL")?, http:reqwest::Client::new(),
        nats:nats.clone(), auth:Some(auth), switches:Arc::new(RwLock::new(HashMap::new())),
    };
    let app = Router::new().route("/play", get(play_handler)).with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("ws://{}/play", listener.local_addr()?);
    let gateway_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut first = enter(&url, &tokens["valid"], "Integration North").await?;
    let initial = state_for(&mut first, "Integration North").await?;
    let first_pid = initial["me"].as_str().unwrap().to_owned();
    let town = initial["towns"].as_array().unwrap().iter().find(|t| t["mayor_player_id"] == first_pid).unwrap();
    let hall = town["hall"]["origin"].clone();
    let player = initial["players"].as_array().unwrap().iter().find(|p| p[0] == first_pid).unwrap();
    assert_eq!(player[1].as_f64().unwrap(), (hall["x"].as_i64().unwrap() + 1) as f64 * 48.0);
    assert_eq!(player[2].as_f64().unwrap(), (hall["y"].as_i64().unwrap() + 3) as f64 * 48.0);
    let mut second = enter(&url, &tokens["valid"], "Integration South").await?;
    let second_state = state_for(&mut second, "Integration South").await?;
    let second_pid = second_state["me"].as_str().unwrap().to_owned();
    assert_ne!(first_pid, second_pid);
    let both = state_for(&mut first, "Integration South").await?;
    let other_hall = &both["towns"].as_array().unwrap().iter().find(|t| t["mayor_player_id"] == second_pid).unwrap()["hall"]["origin"];
    assert_ne!(&hall, other_hall);
    drop(first);
    timeout(Duration::from_secs(6), async {
        loop {
            let reply = nats.request(twz_shared::SUBJ_TOWN_LIST, "".into()).await.unwrap();
            let towns: Vec<Town> = serde_json::from_slice(&reply.payload).unwrap();
            if !towns.iter().any(|town| town.mayor_player_id.to_string() == first_pid) { break; }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await?;
    let mut third = enter(&url, &tokens["valid"], "Integration Fresh").await?;
    let fresh = state_for(&mut third, "Integration Fresh").await?;
    assert_ne!(fresh["me"], first_pid);
    let fresh_town = fresh["towns"].as_array().unwrap().iter().find(|t| t["mayor_player_id"] == fresh["me"]).unwrap();
    assert_eq!(fresh_town["hall"]["origin"], hall);
    drop(second);
    drop(third);
    tokio::time::sleep(Duration::from_millis(200)).await;
    gateway_task.abort();
    Ok(())
}

// Exercise the real relay over TCP/WebSockets, without requiring conductor or NATS.
struct Gateway {
    url: String,
    switches: mpsc::UnboundedSender<String>,
    finished: oneshot::Receiver<()>,
    task: JoinHandle<()>,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn gateway(initial: String, replacement: String) -> Gateway {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let (switches, switch_rx) = mpsc::unbounded_channel();
    let (finished_tx, finished) = oneshot::channel();
    let session = Arc::new(tokio::sync::Mutex::new(Some((switch_rx, finished_tx))));
    let app = Router::new().route("/ws", get(move |ws: WebSocketUpgrade| {
        let initial = initial.clone();
        let replacement = replacement.clone();
        let session = session.clone();
        async move {
            ws.on_upgrade(move |client| async move {
                let (switch_rx, finished_tx) = session.lock().await.take().unwrap();
                let (backend, _) = connect_async(format!("{initial}/ws")).await.unwrap();
                relay_session(client, backend, "00000000-0000-0000-0000-000000000001", None, switch_rx, move |_| {
                    let replacement = replacement.clone();
                    async move { Ok(replacement) }
                }).await;
                let _ = finished_tx.send(());
            })
        }
    }));
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Gateway { url, switches, finished, task }
}

async fn connect(url: &str) -> BackendSocket {
    let (mut ws, _) = connect_async(url).await.unwrap();
    let message = timeout(TEST_TIMEOUT, ws.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(message.into_text().unwrap(), "ready");
    ws
}

#[tokio::test]
async fn browser_close_disconnects_backend_and_finishes_relay() {
    let mut backend = backend().await;
    let mut gateway = gateway(backend.url.clone(), String::new()).await;
    let mut client = connect(&gateway.url).await;
    assert_eq!(timeout(TEST_TIMEOUT, &mut backend.connected).await.unwrap().unwrap(), "00000000-0000-0000-0000-000000000001");
    let identity = timeout(TEST_TIMEOUT, &mut backend.identity).await.unwrap().unwrap();
    assert_eq!(identity["identity"]["game_id"], "00000000-0000-0000-0000-000000000001");
    assert!(identity["identity"]["town_id"].is_null());
    assert!(identity["identity"]["role"].is_null());
    client.close(None).await.unwrap();
    timeout(TEST_TIMEOUT, &mut backend.disconnected).await.unwrap().unwrap();
    timeout(TEST_TIMEOUT, &mut gateway.finished).await.unwrap().unwrap();
}

#[tokio::test]
async fn catalog_endpoint_exposes_versioned_readonly_definitions() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/catalog", listener.local_addr()?);
    let app = Router::new().route("/catalog", get(catalog_handler));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = reqwest::Client::new();
    let catalog: serde_json::Value = http.get(&url).send().await?.error_for_status()?.json().await?;
    assert_eq!(catalog["version"], twz_shared::DOMAIN_VERSION);
    assert_eq!(catalog["buildings"].as_array().unwrap().len(), 28);
    assert_eq!(catalog["items"].as_array().unwrap().len(), 13);
    assert_eq!(catalog["item_buildings"].as_array().unwrap().len(), 4);
    assert_eq!(http.post(&url).send().await?.status(), axum::http::StatusCode::METHOD_NOT_ALLOWED);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn abrupt_browser_drop_disconnects_backend_and_finishes_relay() {
    let mut backend = backend().await;
    let mut gateway = gateway(backend.url.clone(), String::new()).await;
    let client = connect(&gateway.url).await;
    // Drop TCP without sending a WebSocket close frame, as during reload/abort.
    drop(client);
    timeout(TEST_TIMEOUT, &mut backend.disconnected).await.unwrap().unwrap();
    timeout(TEST_TIMEOUT, &mut gateway.finished).await.unwrap().unwrap();
}

#[tokio::test]
async fn browser_drop_during_setup_does_not_leave_backend_connected() {
    let mut backend = backend().await;
    let mut gateway = gateway(backend.url.clone(), String::new()).await;
    // React development effect cleanup can close before the first game frame.
    let (client, _) = connect_async(&gateway.url).await.unwrap();
    drop(client);
    timeout(TEST_TIMEOUT, &mut backend.disconnected).await.unwrap().unwrap();
    timeout(TEST_TIMEOUT, &mut gateway.finished).await.unwrap().unwrap();
}

#[tokio::test]
async fn backend_failure_closes_browser_and_finishes_relay() {
    let backend = backend().await;
    let mut gateway = gateway(backend.url.clone(), String::new()).await;
    let mut client = connect(&gateway.url).await;
    backend.task.abort();
    let message = timeout(TEST_TIMEOUT, client.next()).await.unwrap();
    assert!(matches!(message, Some(Ok(TMsg::Close(_))) | Some(Err(_)) | None));
    timeout(TEST_TIMEOUT, &mut gateway.finished).await.unwrap().unwrap();
}

#[tokio::test]
async fn handoff_closes_old_backend_and_browser_drop_closes_replacement() {
    let mut first = backend().await;
    let mut second = backend().await;
    let mut gateway = gateway(first.url.clone(), second.url.clone()).await;
    let mut client = connect(&gateway.url).await;
    gateway.switches.send("second".into()).unwrap();
    assert_eq!(timeout(TEST_TIMEOUT, &mut second.connected).await.unwrap().unwrap(), "00000000-0000-0000-0000-000000000001");
    timeout(TEST_TIMEOUT, &mut first.disconnected).await.unwrap().unwrap();
    let message = timeout(TEST_TIMEOUT, client.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(message.into_text().unwrap(), "ready");
    drop(client);
    timeout(TEST_TIMEOUT, &mut second.disconnected).await.unwrap().unwrap();
    timeout(TEST_TIMEOUT, &mut gateway.finished).await.unwrap().unwrap();
}
