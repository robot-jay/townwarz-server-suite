//! Isolated Phase 3 acceptance: real service children, PostgreSQL commit barriers,
//! signed fixture authentication, and (optionally) an actual Chromium client.
//! No test/fault endpoints are added to production services.
use super::*;
use anyhow::Context;
use sea_orm::{
    ConnectionTrait, Database, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait,
};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};
use twz_shared::*;

const COMMIT_GATE: i64 = 7_493_301;
const SUBJECT: &str = "auth0|test-mayor";

struct Service {
    child: Option<Child>,
    executable: PathBuf,
    arguments: Vec<String>,
    database: String,
    income: u64,
    log: PathBuf,
}
impl Service {
    fn start(&mut self) -> Result<()> {
        let output = File::options().create(true).append(true).open(&self.log)?;
        self.child = Some(
            Command::new(&self.executable)
                .args(&self.arguments)
                .env("DATABASE_URL", &self.database)
                .env("TWZ_MAYOR_INCOME_AMOUNT", self.income.to_string())
                .env("TWZ_MAYOR_INCOME_INTERVAL_MS", "1000")
                .stdin(Stdio::null())
                .stdout(output.try_clone()?)
                .stderr(output)
                .spawn()
                .with_context(|| format!("start {}", self.executable.display()))?,
        );
        Ok(())
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Suite {
    conductor: Service,
    cells: Vec<Service>,
    cell_ids: Vec<String>,
    cell_urls: Vec<String>,
    conductor_url: String,
    gateway_url: String,
    token: String,
    nats: async_nats::Client,
    db: DatabaseConnection,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    root: PathBuf,
}
impl Drop for Suite {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}
fn sql(text: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, text, values)
}

impl Suite {
    async fn new(name: &str, income: u64) -> Result<Self> {
        let database =
            std::env::var("TEST_DATABASE_URL").context("use an isolated TEST_DATABASE_URL")?;
        let nats_url = std::env::var("TEST_NATS_URL")
            .context("use an isolated TEST_NATS_URL without another conductor")?;
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let bins = std::env::var_os("TEST_SERVICE_BIN_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target/debug"));
        let logs = root.join("target/ci-logs").join(name);
        fs::create_dir_all(&logs)?;
        let cp = port()?;
        let conductor_url = format!("http://127.0.0.1:{cp}");
        let program = |name: &str| bins.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        // Each independently managed suite explicitly gates its rollout. This
        // remains safe after the outer CI migration step because history makes
        // the command idempotent; the conductor itself must perform no DDL.
        let migration = Command::new(program("twz-migrate"))
            .arg("up")
            .env("DATABASE_URL", &database)
            .status()
            .context("run deployment migration command before acceptance services")?;
        anyhow::ensure!(
            migration.success(),
            "deployment migrations failed; refusing to start acceptance services"
        );
        let mut conductor = Service {
            child: None,
            executable: program("twz-conductor"),
            arguments: vec![
                "--bind".into(),
                format!("127.0.0.1:{cp}"),
                "--nats".into(),
                nats_url.clone(),
            ],
            database: database.clone(),
            income,
            log: logs.join("conductor.log"),
        };
        conductor.start()?;
        let nats = async_nats::connect(&nats_url).await?;
        ready_registry(&nats).await?;
        ready_http(&format!("{conductor_url}/health")).await?;
        let db = Database::connect(&database).await?;
        // Only the signed fixture account is retired; repository tests have
        // separate subjects. This allows failed cases to leave useful history.
        db.execute(sql("UPDATE twz_games SET ended_at=NOW(), end_reason='intentional_quit', connection_id=NULL WHERE auth0_sub=$1 AND ended_at IS NULL", vec![SUBJECT.into()])).await?;
        db.execute_unprepared("DROP TRIGGER IF EXISTS twz_acceptance_gate ON twz_games; DROP FUNCTION IF EXISTS twz_acceptance_gate()").await?;
        let mut cells = Vec::new();
        let mut cell_ids = Vec::new();
        let mut cell_urls = Vec::new();
        for label in ["AcceptanceA", "AcceptanceB"] {
            let p = port()?;
            let address = format!("127.0.0.1:{p}");
            let mut cell = Service {
                child: None,
                executable: program("twz-cellserver"),
                arguments: vec![
                    "--id".into(),
                    label.into(),
                    "--bind".into(),
                    address.clone(),
                    "--advertise".into(),
                    address.clone(),
                    "--conductor".into(),
                    conductor_url.clone(),
                    "--nats".into(),
                    nats_url.clone(),
                ],
                database: database.clone(),
                income,
                log: logs.join(format!("{label}.log")),
            };
            cell.start()?;
            ready_http(&format!("http://{address}/health")).await?;
            cell_ids.push(label.to_owned());
            cell_urls.push(format!("http://{address}"));
            cells.push(cell);
        }
        let (auth, tokens) = auth::Auth0::fixture();
        let state = AppState {
            conductor: conductor_url.clone(),
            http: reqwest::Client::new(),
            nats: nats.clone(),
            auth: Some(auth),
            switches: Arc::new(RwLock::new(HashMap::new())),
        };
        let switches = tokio::spawn(global_switch_listener(state.clone()));
        let app = Router::new()
            .route("/play", get(play_handler))
            .route("/catalog", get(catalog_handler))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let gateway_url = format!("ws://{}/play", listener.local_addr()?);
        let gateway = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        nats.flush().await?;
        Ok(Self {
            conductor,
            cells,
            cell_ids,
            cell_urls,
            conductor_url,
            gateway_url,
            token: tokens["valid"].as_str().unwrap().to_owned(),
            nats,
            db,
            tasks: vec![switches, gateway],
            root,
        })
    }
    async fn connect(&self, resume: bool) -> Result<BackendSocket> {
        let (mut client, _) = connect_async(&self.gateway_url).await?;
        let value = if resume {
            serde_json::json!({"type":"resume_game","id_token":self.token})
        } else {
            serde_json::json!({"type":"start_game","role":"mayor","town_name":"Acceptance Town","id_token":self.token})
        };
        client.send(TMsg::Text(value.to_string())).await?;
        Ok(client)
    }
    async fn start(&self) -> Result<(BackendSocket, ClientId, Town)> {
        let mut client = self.connect(false).await?;
        let frame = frame(&mut client, "state", |_| true).await?;
        let id: ClientId = frame["me"].as_str().unwrap().parse()?;
        let town = serde_json::from_value(
            frame["towns"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["mayor_player_id"] == id.to_string())
                .unwrap()
                .clone(),
        )?;
        frame_economy(&mut client).await?;
        Ok((client, id, town))
    }
    async fn resume(&self) -> Result<BackendSocket> {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let mut client = self.connect(true).await?;
                if frame(&mut client, "state", |_| true).await.is_ok() {
                    return Ok(client);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await?
    }
    async fn economy(&self, id: ClientId) -> Result<EconomyState> {
        let row = self
            .db
            .query_one(sql(
                "SELECT economy_json FROM twz_games WHERE player_id=$1",
                vec![id.to_string().into()],
            ))
            .await?
            .context("game row")?;
        Ok(serde_json::from_value(
            row.try_get::<serde_json::Value>("", "economy_json")?,
        )?)
    }
    async fn seed_cash(&self, client: &mut BackendSocket, id: ClientId, cash: u64) -> Result<()> {
        self.db.execute(sql("UPDATE twz_games SET economy_json=jsonb_set(economy_json,'{cash}',$2::jsonb) WHERE player_id=$1", vec![id.to_string().into(), serde_json::json!(cash).into()])).await?;
        frame(client, "economy_state", |v| {
            v["cash"].as_u64().is_some_and(|actual| actual >= cash)
        })
        .await?;
        Ok(())
    }
    async fn disconnected(&self, id: ClientId) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let row = self
                    .db
                    .query_one(sql(
                        "SELECT connection_id FROM twz_games WHERE player_id=$1",
                        vec![id.to_string().into()],
                    ))
                    .await?
                    .unwrap();
                if row
                    .try_get::<Option<String>>("", "connection_id")?
                    .is_none()
                {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await?
    }
    async fn owner(&self, town: &Town) -> Result<usize> {
        let response: RouteCellResponse = reqwest::get(format!(
            "{}/route/cell/{}/{}",
            self.conductor_url, town.hall.cell.0, town.hall.cell.1
        ))
        .await?
        .error_for_status()?
        .json()
        .await?;
        self.cell_ids
            .iter()
            .position(|id| id == &response.owner_server_id)
            .context("fixture owner")
    }
    async fn restart_conductor(&mut self) -> Result<()> {
        self.conductor.stop();
        self.conductor.start()?;
        ready_registry(&self.nats).await?;
        ready_http(&format!("{}/health", self.conductor_url)).await?;
        for id in &self.cell_ids {
            ready_http(&format!("{}/server/{id}", self.conductor_url)).await?;
        }
        Ok(())
    }
    async fn restart_cell(&mut self, index: usize) -> Result<()> {
        self.cells[index].stop();
        self.cells[index].start()?;
        ready_http(&format!("{}/health", self.cell_urls[index])).await
    }
    async fn gate(&self, command: &ConstructionCommand) -> Result<DatabaseTransaction> {
        // A trigger pauses only THIS request's UPDATE after spatial preparation,
        // while the test's separate connection owns the advisory lock. Killing
        // the conductor here exercises real PostgreSQL rollback, not a mock.
        let gate = self.db.begin().await?;
        gate.execute(sql(
            "SELECT pg_advisory_xact_lock($1)",
            vec![COMMIT_GATE.into()],
        ))
        .await?;
        self.db
            .execute_unprepared(&format!(
                r#"
            CREATE OR REPLACE FUNCTION twz_acceptance_gate() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
              IF NEW.economy_json->'outcomes' ? '{}' THEN
                PERFORM pg_advisory_xact_lock({COMMIT_GATE});
              END IF;
              RETURN NEW;
            END $$;
            CREATE TRIGGER twz_acceptance_gate BEFORE UPDATE OF economy_json ON twz_games
            FOR EACH ROW EXECUTE FUNCTION twz_acceptance_gate();
        "#,
                command.request_id()
            ))
            .await?;
        Ok(gate)
    }
    async fn gated(&self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let row = self.db.query_one(sql("SELECT COUNT(*)::bigint AS waiting FROM pg_locks WHERE locktype='advisory' AND objid::bigint=$1 AND NOT granted", vec![COMMIT_GATE.into()])).await?.unwrap();
                if row.try_get::<i64>("", "waiting")? > 0 { return Ok::<_, anyhow::Error>(()); }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.context("conductor did not reach the precommit barrier")?
    }
    async fn clear_gate(&self) -> Result<()> {
        self.db.execute_unprepared("DROP TRIGGER IF EXISTS twz_acceptance_gate ON twz_games; DROP FUNCTION IF EXISTS twz_acceptance_gate()").await?;
        Ok(())
    }
    async fn resume_with_lost_ack(
        &mut self,
        request_id: ClientId,
    ) -> Result<(BackendSocket, Arc<std::sync::atomic::AtomicBool>)> {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dropped = Arc::new(AtomicBool::new(false));
        let observed = dropped.clone();
        let upstream = self.gateway_url.clone();
        let app = Router::new().route("/play", get(move |ws: WebSocketUpgrade| {
            let upstream = upstream.clone(); let observed = observed.clone();
            async move { ws.on_upgrade(move |client| async move {
                let (backend, _) = connect_async(&upstream).await.unwrap();
                let (mut bs, mut br) = backend.split();
                let (mut cs, mut cr) = client.split();
                loop {
                    tokio::select! {
                        message = cr.next() => {
                            let Some(Ok(message)) = message else { break; };
                            let Some(message) = axum_to_tungstenite(message) else { break; };
                            if bs.send(message).await.is_err() { break; }
                        }
                        message = br.next() => {
                            let Some(Ok(message)) = message else { break; };
                            if let TMsg::Text(text) = message {
                                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                                    if value["type"] == "construction_result" && value["result"]["request_id"] == request_id.to_string() {
                                        observed.store(true, Ordering::SeqCst);
                                        continue;
                                    }
                                }
                                if cs.send(AxumMsg::Text(text)).await.is_err() { break; }
                            } else if matches!(message, TMsg::Close(_)) { break; }
                        }
                    }
                }
                let _ = bs.send(TMsg::Close(None)).await;
            }) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}/play", listener.local_addr()?);
        self.tasks.push(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap()
        }));
        let (mut client, _) = connect_async(url).await?;
        client
            .send(TMsg::Text(
                serde_json::json!({"type":"resume_game","id_token":self.token}).to_string(),
            ))
            .await?;
        frame(&mut client, "state", |_| true).await?;
        Ok((client, dropped))
    }
}

async fn ready_registry(nats: &async_nats::Client) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if matches!(
                tokio::time::timeout(
                    Duration::from_millis(300),
                    nats.request(SUBJ_TOWN_LIST, "".into())
                )
                .await,
                Ok(Ok(_))
            ) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("conductor registry readiness")
}
async fn ready_http(url: &str) -> Result<()> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()?;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if matches!(http.get(url).send().await, Ok(response) if response.status().is_success())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .with_context(|| format!("readiness: {url}"))
}
async fn frame(
    client: &mut BackendSocket,
    kind: &str,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> Result<serde_json::Value> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let message = client.next().await.context("browser socket closed")??;
            if let TMsg::Text(text) = message {
                let value: serde_json::Value = serde_json::from_str(&text)?;
                if value["type"] == "game_error" {
                    anyhow::bail!("{}", value["message"]);
                }
                if value["type"] == kind && predicate(&value) {
                    return Ok(value);
                }
            } else if matches!(message, TMsg::Close(_)) {
                anyhow::bail!("browser socket closed");
            }
        }
    })
    .await
    .with_context(|| format!("waiting for {kind}"))?
}
async fn frame_economy(client: &mut BackendSocket) -> Result<serde_json::Value> {
    frame(client, "economy_state", |_| true).await
}
async fn send(client: &mut BackendSocket, command: &ConstructionCommand) -> Result<()> {
    client
        .send(TMsg::Text(serde_json::to_string(command)?))
        .await?;
    Ok(())
}
async fn outcome(
    client: &mut BackendSocket,
    command: &ConstructionCommand,
) -> Result<serde_json::Value> {
    frame(client, "construction_result", |v| {
        v["result"]["request_id"] == command.request_id().to_string()
    })
    .await
}
fn research(town: &Town) -> ConstructionCommand {
    ConstructionCommand::PlaceBuilding {
        request_id: ClientId::new_v4(),
        building_type: BuildingType::WallResearch,
        origin_tile: IVec2 {
            x: town.hall.origin.x - 3,
            y: town.hall.origin.y,
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated TEST_DATABASE_URL/TEST_NATS_URL, built service binaries; owns/restarts its service children"]
async fn phase3_acceptance_precommit_crash_rolls_back_reservation_and_retries_once() -> Result<()> {
    let mut suite = Suite::new("phase3-precommit", 0).await?;
    let (mut client, id, town) = suite.start().await?;
    suite.seed_cash(&mut client, id, 100000).await?;
    let command = research(&town);
    let gate = suite.gate(&command).await?;
    send(&mut client, &command).await?;
    suite.gated().await?;
    let before = suite.economy(id).await?;
    assert_eq!(before.cash, 100000);
    assert!(before.buildings.is_empty());
    assert!(before.replay(&command).is_none());
    frame(&mut client, "state", |v| {
        v["construction"]["reason"] == "busy"
    })
    .await?;
    suite.conductor.stop();
    gate.rollback().await?;
    suite.clear_gate().await?;
    suite.restart_conductor().await?;
    let lost = outcome(&mut client, &command).await?;
    assert_eq!(lost["outcome_known"], false);
    frame(&mut client, "state", |v| {
        v["construction"]["can_construct"] == true
    })
    .await?;
    assert_eq!(suite.economy(id).await?.cash, 100000);
    send(&mut client, &command).await?;
    let success = outcome(&mut client, &command).await?;
    assert_eq!(success["outcome_known"], true);
    assert!(success["result"]["error"].is_null(), "{success}");
    send(&mut client, &command).await?;
    assert_eq!(
        outcome(&mut client, &command).await?["result"],
        success["result"]
    );
    let after = suite.economy(id).await?;
    assert_eq!(after.cash, 95000);
    assert_eq!(after.buildings.len(), 1);
    client
        .send(TMsg::Text("{\"type\":\"quit_game\"}".into()))
        .await?;
    frame(&mut client, "game_quit_ack", |_| true).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated TEST_DATABASE_URL/TEST_NATS_URL, built service binaries; owns/restarts its service children"]
async fn phase3_acceptance_commit_without_apply_or_ack_recovers_and_never_resurrects() -> Result<()>
{
    let mut suite = Suite::new("phase3-postcommit", 0).await?;
    let (mut client, id, town) = suite.start().await?;
    suite.seed_cash(&mut client, id, 100000).await?;
    let owner = suite.owner(&town).await?;
    let command = research(&town);
    let gate = suite.gate(&command).await?;
    send(&mut client, &command).await?;
    suite.gated().await?;
    // Kill the prepared cell before releasing the durable commit, then wait for
    // the browser socket to close: neither apply nor acknowledgement can arrive.
    suite.cells[owner].stop();
    tokio::time::timeout(Duration::from_secs(12), async {
        while let Some(message) = client.next().await {
            if !matches!(message, Ok(TMsg::Text(_))) {
                break;
            }
        }
    })
    .await?;
    gate.commit().await?;
    suite.clear_gate().await?;
    suite.disconnected(id).await?;
    let committed = suite.economy(id).await?;
    assert_eq!(committed.cash, 95000);
    assert_eq!(committed.buildings.len(), 1);
    let building_id = committed.buildings[0].building.id;
    // Deliberately replace movement cash with an old value. Resume must restore
    // economic truth, not roll back the paid building's debit.
    suite.db.execute(sql("UPDATE twz_games SET checkpoint_json=jsonb_set(checkpoint_json,'{game,wallet,cash}','999999'::jsonb) WHERE player_id=$1", vec![id.to_string().into()])).await?;
    suite.restart_cell(owner).await?;
    let mut resumed = suite.resume().await?;
    let restored = frame(&mut resumed, "state", |v| {
        v["buildings"]
            .as_array()
            .is_some_and(|bs| bs.iter().any(|b| b["id"] == building_id.to_string()))
    })
    .await?;
    assert_eq!(restored["self_state"]["wallet"]["cash"], 95000);
    send(&mut resumed, &command).await?;
    assert_eq!(
        outcome(&mut resumed, &command).await?["result"]["building_id"],
        building_id.to_string()
    );
    // A legacy-price fixture differs from today's 5,000 catalog price and is
    // not divisible by four. The real demolition handler must use provenance.
    suite.db.execute(sql("UPDATE twz_games SET economy_json=jsonb_set(economy_json,'{buildings,0,price_paid}','8003'::jsonb) WHERE player_id=$1", vec![id.to_string().into()])).await?;
    let demolition = ConstructionCommand::DemolishBuilding {
        request_id: ClientId::new_v4(),
        building_id,
    };
    send(&mut resumed, &demolition).await?;
    let demolished = outcome(&mut resumed, &demolition).await?;
    assert_eq!(demolished["result"]["refund"], 2000);
    send(&mut resumed, &demolition).await?;
    assert_eq!(
        outcome(&mut resumed, &demolition).await?["result"],
        demolished["result"]
    );
    assert_eq!(suite.economy(id).await?.cash, 97000);
    assert!(suite.economy(id).await?.buildings.is_empty());
    // Restart after demolition and replay the OLD successful purchase: its
    // historical result must not recreate a building that was later removed.
    drop(resumed);
    suite.disconnected(id).await?;
    suite.restart_cell(owner).await?;
    let mut resumed = suite.resume().await?;
    send(&mut resumed, &command).await?;
    outcome(&mut resumed, &command).await?;
    assert!(suite.economy(id).await?.buildings.is_empty());
    let final_state = frame(&mut resumed, "state", |_| true).await?;
    assert!(!final_state["buildings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["id"] == building_id.to_string()));
    // Lose ONLY the client acknowledgement after a successful commit AND live
    // simulation application. The proxy is test-only; all game services are real.
    drop(resumed);
    suite.disconnected(id).await?;
    let last = research(&town);
    let (mut lossy, dropped) = suite.resume_with_lost_ack(last.request_id()).await?;
    send(&mut lossy, &last).await?;
    let applied = frame(&mut lossy, "state", |v| {
        v["buildings"]
            .as_array()
            .is_some_and(|bs| bs.iter().any(|b| b["building_type"] == "wall_research"))
    })
    .await?;
    assert!(applied["construction"]["can_construct"].as_bool().unwrap());
    tokio::time::timeout(Duration::from_secs(5), async {
        while !dropped.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let after_lost_ack = suite.economy(id).await?;
    drop(lossy);
    suite.disconnected(id).await?;
    let mut resumed = suite.resume().await?;
    send(&mut resumed, &last).await?;
    outcome(&mut resumed, &last).await?;
    let replayed = suite.economy(id).await?;
    assert_eq!(replayed.cash, after_lost_ack.cash);
    assert_eq!(replayed.buildings.len(), 1);
    assert_eq!(
        replayed.buildings[0].building.id,
        after_lost_ack.buildings[0].building.id
    );
    // End participation, restart the cell, then deliver an old apply.
    let old_projection = EconomyProjection::new(town.clone(), &suite.economy(id).await?);
    resumed
        .send(TMsg::Text("{\"type\":\"quit_game\"}".into()))
        .await?;
    frame(&mut resumed, "game_quit_ack", |_| true).await?;
    suite.restart_cell(owner).await?;
    suite
        .nats
        .publish(
            subj_economy_apply(&suite.cell_ids[owner]),
            serde_json::to_vec(&old_projection)?.into(),
        )
        .await?;
    suite.nats.flush().await?;
    let observer_id = ClientId::new_v4();
    let mut position = PlayerState::initial_for_town(&town);
    position.id = observer_id;
    position.game = PlayerGameState::new(GameIdentity::debug(observer_id));
    let url = suite.cell_urls[owner].replacen("http", "ws", 1) + "/ws";
    let (mut observer, _) = connect_async(url).await?;
    observer
        .send(TMsg::Text(serde_json::to_string(&BackendHello::SetPid {
            id: observer_id,
            town: None,
            identity: Some(position.game.identity.clone()),
            resume_state: Some(position),
        })?))
        .await?;
    let observed = frame(&mut observer, "state", |v| {
        v["buildings"].as_array().is_some_and(|bs| {
            bs.iter()
                .any(|b| b["id"] == hall_entity_id(town.hall.origin).to_string())
        })
    })
    .await?;
    let buildings = observed["buildings"].as_array().unwrap();
    assert!(buildings
        .iter()
        .filter(|b| b["building_type"] == "town_hall")
        .all(|b| b["ownership"].is_null()));
    assert!(!buildings
        .iter()
        .any(|b| b["ownership"]["game_id"] == id.to_string()));
    // Install a legitimate new participation at the same physical site using
    // fixture allocation data, then redeliver the old game's apply/cleanup.
    let next_id = ClientId::new_v4();
    let reply = suite
        .nats
        .request(
            SUBJ_TOWN_CREATE,
            serde_json::to_vec(&CreateTownRequest {
                player_id: next_id,
                desired_name: "Hall Reused".into(),
                user: GameUser {
                    sub: format!("acceptance|{next_id}"),
                    name: None,
                    email: None,
                },
            })?
            .into(),
        )
        .await?;
    let admission: CreateTownResponse = serde_json::from_slice(&reply.payload)?;
    assert!(admission.ok, "{:?}", admission.error);
    let mut next_town = admission.town.unwrap();
    next_town.hall = town.hall.clone();
    next_town.created_at_ms = next_town.created_at_ms.max(town.created_at_ms + 1);
    suite.db.execute(sql("UPDATE twz_games SET hall_slot=(SELECT hall_slot FROM twz_games WHERE player_id=$1), town_json=$3, checkpoint_json=$4 WHERE player_id=$2", vec![id.to_string().into(), next_id.to_string().into(), serde_json::to_value(&next_town)?.into(), serde_json::to_value(PlayerState::initial_for_town(&next_town))?.into()])).await?;
    frame(&mut observer, "state", |v| {
        v["buildings"].as_array().is_some_and(|bs| {
            bs.iter().any(|b| {
                b["id"] == hall_entity_id(town.hall.origin).to_string()
                    && b["ownership"]["game_id"] == next_id.to_string()
            })
        })
    })
    .await?;
    let sent_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    suite
        .nats
        .publish(
            subj_economy_apply(&suite.cell_ids[owner]),
            serde_json::to_vec(&old_projection)?.into(),
        )
        .await?;
    suite
        .nats
        .publish(
            SUBJ_GAME_ENDED,
            serde_json::to_vec(&GameEnded {
                player_id: id,
                reason: GameEndReason::Timeout,
            })?
            .into(),
        )
        .await?;
    suite.nats.flush().await?;
    let preserved = frame(&mut observer, "state", |v| {
        v["t"].as_u64().is_some_and(|t| t > sent_at + 200)
    })
    .await?;
    let physical_hall = preserved["buildings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == hall_entity_id(town.hall.origin).to_string())
        .unwrap();
    assert_eq!(physical_hall["ownership"]["game_id"], next_id.to_string());
    assert_eq!(physical_hall["town_name"], "Hall Reused");
    play::operation(
        &suite.nats,
        SUBJ_GAME_END,
        &EndGameRequest {
            player_id: next_id,
            connection_id: None,
            reason: GameEndReason::IntentionalQuit,
        },
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated TEST_DATABASE_URL/TEST_NATS_URL and built service binaries"]
async fn phase3_acceptance_competing_placements_and_income_through_respawn_reconnect_handoff(
) -> Result<()> {
    let suite = Suite::new("phase3-income", 100).await?;
    let (mut client, id, town) = suite.start().await?;
    frame(&mut client, "economy_state", |v| {
        v["cash"].as_u64().unwrap_or(0) >= 100
    })
    .await?;
    drop(client);
    suite.disconnected(id).await?;
    let offline = suite.economy(id).await?;
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(suite.economy(id).await?.cash, offline.cash);
    assert_eq!(
        suite.economy(id).await?.income_progress_ms,
        offline.income_progress_ms
    );
    let respawn = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64
        + 3500;
    suite.db.execute(sql("UPDATE twz_games SET checkpoint_json=jsonb_set(jsonb_set(checkpoint_json,'{hp}','0'::jsonb),'{respawn_at}',$2::jsonb) WHERE player_id=$1", vec![id.to_string().into(), serde_json::json!(respawn).into()])).await?;
    let mut client = suite.resume().await?;
    let dead = frame(&mut client, "state", |v| {
        v["self_state"].is_object()
            && v["players"]
                .as_array()
                .is_some_and(|ps| ps.iter().any(|p| p[0] == id.to_string() && p[4] == 0))
    })
    .await?;
    assert_eq!(dead["construction"]["reason"], "not_alive");
    frame(&mut client, "economy_state", |v| {
        v["cash"].as_u64().unwrap_or(0) > offline.cash
    })
    .await?;
    let state = suite.economy(id).await?;
    assert_eq!(
        state.cash,
        offline.cash + ((offline.income_progress_ms + state.connection_elapsed_ms) / 1000) * 100
    );
    let alive = frame(&mut client, "state", |v| {
        v["construction"]["can_construct"] == true
    })
    .await?;
    let before_handoff = suite.economy(id).await?;
    let home = suite.owner(&town).await?;
    let target = 1 - home;
    let mut destination = None;
    for x in 0..20 {
        let owner: RouteCellResponse =
            reqwest::get(format!("{}/route/cell/{x}/0", suite.conductor_url))
                .await?
                .json()
                .await?;
        if owner.owner_server_id == suite.cell_ids[target] {
            destination = Some(x);
            break;
        }
    }
    let x = destination.context("find other cell authority")?;
    let mut player: PlayerState = serde_json::from_value(alive["resume_state"].clone())?;
    player.x = (x * CELL_TILES + TOWN_OFFSET + 1) as f32 * TILE_PX;
    player.y = (TOWN_OFFSET + 3) as f32 * TILE_PX;
    // Drive the existing trusted handoff transport deterministically; the
    // ordinary source-freeze/crossing path is covered by the cell handoff test.
    let mut acknowledgements = suite
        .nats
        .subscribe(format!("handoff.{}.ack", suite.cell_ids[home]))
        .await?;
    suite.nats.flush().await?;
    suite
        .nats
        .publish(
            format!("handoff.{}.req", suite.cell_ids[target]),
            serde_json::to_vec(&HandoffRequest {
                from_server: suite.cell_ids[home].clone(),
                to_server: suite.cell_ids[target].clone(),
                player,
                ts_ms: respawn,
            })?
            .into(),
        )
        .await?;
    let ack = tokio::time::timeout(Duration::from_secs(5), acknowledgements.next())
        .await?
        .unwrap();
    assert!(serde_json::from_slice::<HandoffAck>(&ack.payload)?.accepted);
    suite
        .nats
        .publish(
            format!("switch.{id}"),
            serde_json::json!({"to_server":suite.cell_ids[target]})
                .to_string()
                .into(),
        )
        .await?;
    frame(&mut client, "state", |v| {
        v["resume_state"]["x"].as_f64().unwrap_or(0.0)
            >= x as f64 * CELL_TILES as f64 * TILE_PX as f64
            && v["construction"]["reason"] == "outside_territory"
    })
    .await?;
    frame_economy(&mut client).await?;
    let after = suite.economy(id).await?;
    assert_eq!(after.income_connection, before_handoff.income_connection);
    assert!(after.connection_elapsed_ms >= before_handoff.connection_elapsed_ms);
    assert_eq!(
        after.cash,
        offline.cash + ((offline.income_progress_ms + after.connection_elapsed_ms) / 1000) * 100
    );
    // Fund this spatial rejection fixture so affordability cannot mask the
    // outside-territory check (normal income assertions above remain unseeded).
    suite.seed_cash(&mut client, id, 100000).await?;
    let outside = research(&town);
    send(&mut client, &outside).await?;
    assert_eq!(
        outcome(&mut client, &outside).await?["result"]["error"],
        "outside_territory"
    );
    // Reconnect back home using an authoritative movement fixture, then submit
    // two intentions before either reply. Exactly one can own the footprint.
    drop(client);
    suite.disconnected(id).await?;
    let mut checkpoint = PlayerState::initial_for_town(&town);
    checkpoint.game.wallet.cash = after.cash;
    suite
        .db
        .execute(sql(
            "UPDATE twz_games SET checkpoint_json=$2 WHERE player_id=$1",
            vec![
                id.to_string().into(),
                serde_json::to_value(checkpoint)?.into(),
            ],
        ))
        .await?;
    let mut client = suite.resume().await?;
    suite.seed_cash(&mut client, id, 100000).await?;
    let balance_before = suite.economy(id).await?;
    let first = research(&town);
    let second = research(&town);
    send(&mut client, &first).await?;
    send(&mut client, &second).await?;
    let a = outcome(&mut client, &first).await?;
    let b = outcome(&mut client, &second).await?;
    assert!(a["result"]["error"].is_null(), "{a}");
    assert_eq!(b["result"]["error"], "occupied");
    send(&mut client, &first).await?;
    assert_eq!(outcome(&mut client, &first).await?["result"], a["result"]);
    let balance_after = suite.economy(id).await?;
    assert_eq!(balance_after.buildings.len(), 1);
    let eligible_delta = balance_after.connection_elapsed_ms - balance_before.connection_elapsed_ms;
    let earned = ((balance_before.income_progress_ms + eligible_delta) / 1000) * 100;
    assert_eq!(
        balance_after.cash,
        balance_before.cash + earned - 5000,
        "one debit only: rejected competing placement and successful replay cannot spend again"
    );
    client
        .send(TMsg::Text("{\"type\":\"quit_game\"}".into()))
        .await?;
    frame(&mut client, "game_quit_ack", |_| true).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL/NATS, built service binaries, npm dependencies and installed Chromium"]
async fn phase3_acceptance_browser_builds_through_real_services() -> Result<()> {
    let suite = Suite::new("phase3-browser", 55000).await?;
    let client = suite.root.join("twz-webclient");
    let browser_port = port()?;
    let mut command = tokio::process::Command::new("node");
    command
        .arg(client.join("node_modules/@playwright/test/cli.js"))
        .arg("test")
        .current_dir(&client)
        .env("CI", "true")
        .env("TEST_REAL_GAME_WS_URL", &suite.gateway_url)
        .env("TEST_REAL_ID_TOKEN", &suite.token)
        .env("TEST_BROWSER_PORT", browser_port.to_string())
        .kill_on_drop(true);
    let mut browser = command
        .spawn()
        .context("launch live Playwright acceptance")?;
    let status = tokio::time::timeout(Duration::from_secs(120), browser.wait()).await??;
    anyhow::ensure!(
        status.success(),
        "live browser acceptance failed; inspect twz-webclient/test-results"
    );
    let row = suite.db.query_one(sql("SELECT economy_json FROM twz_games WHERE auth0_sub=$1 ORDER BY participation_order DESC LIMIT 1", vec![SUBJECT.into()])).await?.unwrap();
    let economy: EconomyState = serde_json::from_value(row.try_get("", "economy_json")?)?;
    assert_eq!(economy.buildings.len(), 1);
    assert_eq!(
        economy.buildings[0].building.building_type,
        BuildingType::WallFactory
    );
    assert_eq!(
        economy.outcomes.len(),
        3,
        "one research, one factory, one demolition; preview/cancel must not send purchases"
    );
    Ok(())
}
