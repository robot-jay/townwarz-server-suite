use super::*;
use twz_shared::{
    ConstructionCommand, ConstructionError, ConstructionResult, EconomyRequest, EconomyResponse,
    SUBJ_ECONOMY,
};

pub(super) struct SessionEconomy {
    tx: mpsc::Sender<EconomyRequest>,
    pub responses: mpsc::Receiver<EconomyResponse>,
    task: tokio::task::JoinHandle<()>,
    started: std::time::Instant,
    connection: GameConnection,
}
impl Drop for SessionEconomy {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl SessionEconomy {
    pub fn new(lease: &RelayLease) -> Self {
        let (tx, mut rx) = mpsc::channel::<EconomyRequest>(16);
        let (out, responses) = mpsc::channel(16);
        let nats = lease.nats.clone();
        let task = tokio::spawn(async move {
            while let Some(request) = rx.recv().await {
                let mut response =
                    request_economy(&nats, &request)
                        .await
                        .unwrap_or_else(|_| EconomyResponse {
                            error: Some(ConstructionError::ServiceUnavailable),
                            result: request.command.as_ref().map(|command| {
                                ConstructionResult::rejected(
                                    command,
                                    ConstructionError::ServiceUnavailable,
                                    0,
                                )
                            }),
                            projection: None,
                        });
                // Coordinator/database errors must also complete the browser's
                // request with its original ID for safe replay after uncertainty.
                if response.result.is_none() {
                    if let Some(command) = &request.command {
                        response.result = Some(ConstructionResult::rejected(
                            command,
                            response
                                .error
                                .unwrap_or(ConstructionError::ServiceUnavailable),
                            response.projection.as_ref().map(|p| p.cash).unwrap_or(0),
                        ));
                    }
                }
                if out.send(response).await.is_err() {
                    break;
                }
            }
        });
        Self {
            tx,
            responses,
            task,
            started: std::time::Instant::now(),
            connection: lease.connection.clone(),
        }
    }
    pub fn request(&self, command: Option<ConstructionCommand>) -> bool {
        self.tx
            .try_send(EconomyRequest {
                player_id: self.connection.player_id,
                connection_id: self.connection.connection_id,
                elapsed_ms: self.elapsed_ms(),
                command,
            })
            .is_ok()
    }
    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
    pub fn stop(&self) {
        self.task.abort();
    }
}
pub(super) async fn request_economy(
    nats: &async_nats::Client,
    request: &EconomyRequest,
) -> Result<EconomyResponse> {
    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        nats.request(SUBJ_ECONOMY, serde_json::to_vec(request)?.into()),
    )
    .await??;
    Ok(serde_json::from_slice(&reply.payload)?)
}
