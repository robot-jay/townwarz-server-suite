use std::time::{SystemTime, UNIX_EPOCH};
use futures_util::StreamExt;
use anyhow::Result;
use async_nats::{Client, Message};
use async_nats::jetstream::{self, kv::Store};
use bytes::Bytes;
use rand::{seq::SliceRandom, thread_rng};
use serde_json;

use twz_shared::{
    CellKey, IVec2, TownHall, Town, CreateTownRequest, CreateTownResponse,
    SUBJ_TOWN_CREATE, spawn_from_hall, subj_cell_spawn, SpawnPlayerAt,
};

const KV_TOWN_HALLS: &str = "town_halls";
const KV_TOWNS: &str = "towns";

pub struct TownRegistry {
    client: Client,
    halls_kv: Store,
    towns_kv: Store,
    halls: Vec<TownHall>,
}

impl TownRegistry {
    pub async fn new(client: Client, halls_from_map: Vec<TownHall>) -> Result<Self> {
        let js = async_nats::jetstream::new(client.clone());

        let halls_kv = js
            .create_key_value(jetstream::kv::Config {
                bucket: KV_TOWN_HALLS.into(),
                ..Default::default()
            })
            .await?;

        let towns_kv = js
            .create_key_value(jetstream::kv::Config {
                bucket: KV_TOWNS.into(),
                ..Default::default()
            })
            .await?;

        Ok(Self { client, halls_kv, towns_kv, halls: halls_from_map })
    }

    pub async fn serve(self) -> Result<()> {
        // sub must be mutable for .next()
        let mut sub = self.client.subscribe(SUBJ_TOWN_CREATE).await?;
        let this = std::sync::Arc::new(self);

        tokio::spawn({
            let this = this.clone();
            async move {
                // rebind as mutable inside the async task
                let mut sub = sub;
                while let Some(msg) = sub.next().await {
                    let resp = this.handle_create_message(&msg).await;
                    if let Some(reply) = msg.reply {
                        let _ = this.client
                            .publish(reply, bytes::Bytes::from(serde_json::to_vec(&resp).unwrap()))
                            .await;
                    }
                }
            }
        });

        Ok(())
    }

    async fn handle_create_message(&self, msg: &Message) -> CreateTownResponse {
        let req: CreateTownRequest = match serde_json::from_slice(&msg.payload) {
            Ok(v) => v,
            Err(e) => {
                return CreateTownResponse {
                    ok: false,
                    error: Some(format!("bad req: {e}")),
                    town: None,
                    spawn_tile: None,
                }
            }
        };

        let mut shuffled = self.halls.clone();
        shuffled.shuffle(&mut rand::thread_rng());

        for hall in shuffled {
            let claim_key = format!("claimed:{}", hall.id);
            if self.try_claim_hall(&claim_key).await {
                let town_id = format!("t-{}", nanoid::nanoid!(10));
                let now_ms = now_ms() as i64;

                let town = Town {
                    id: town_id.clone(),
                    name: req.desired_name.clone(),
                    hall: hall.clone(),
                    created_at_ms: now_ms,
                    mayor_player_id: req.player_id.clone(),
                };

                let _ = self.towns_kv
                    .put(&town_id, Bytes::from(serde_json::to_vec(&town).unwrap()))
                    .await;

                let spawn = spawn_from_hall(hall.origin);

                // Tell the owning cell to spawn the player
                let subject = subj_cell_spawn(&hall.cell);
                let cmd = SpawnPlayerAt { player_id: req.player_id, tile: spawn };
                let _ = self.client
                    .publish(subject, Bytes::from(serde_json::to_vec(&cmd).unwrap()))
                    .await;

                return CreateTownResponse {
                    ok: true,
                    error: None,
                    town: Some(town),
                    spawn_tile: Some(spawn),
                };
            }
        }

        CreateTownResponse {
            ok: false,
            error: Some("no town halls available".into()),
            town: None,
            spawn_tile: None,
        }
    }

    async fn try_claim_hall(&self, key: &str) -> bool {
        match self.halls_kv.get(key).await {
            Ok(Some(_)) => false,
            Ok(None) => self.halls_kv.put(key, Bytes::from_static(b"1")).await.is_ok(),
            Err(_) => false,
        }
    }
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()
}

// Accepts a concrete vector of tiles per cell: (x, y, tileVal)
// tileVal == 3 => Town Hall
pub fn halls_from_map(cells: &[(CellKey, Vec<(i32, i32, u8)>)]) -> Vec<TownHall> {
    let mut out = Vec::new();
    for (cell_key, tiles) in cells {
        for &(x, y, val) in tiles {
            if val == 3 {
                out.push(TownHall {
                    id: format!("h-{}-{}-{}", cell_key.0, cell_key.1, out.len()),
                    cell: *cell_key,
                    origin: IVec2 { x, y },
                });
            }
        }
    }
    out
}