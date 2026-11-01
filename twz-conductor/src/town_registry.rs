use std::{sync::Arc,time::{Duration,SystemTime,UNIX_EPOCH}};
use anyhow::{Context,Result};
use futures_util::StreamExt;
use tokio::sync::Mutex;
use tokio_postgres::{Client,NoTls};
use twz_shared::*;

pub struct TownRegistry { nats:async_nats::Client, db:Mutex<Client> }

impl TownRegistry {
    pub async fn new(nats:async_nats::Client,database_url:&str)->Result<Self> {
        let (db,connection)=tokio_postgres::connect(database_url,NoTls).await.context("connect to TownWarz PostgreSQL database")?;
        tokio::spawn(async move { if let Err(error)=connection.await { tracing::error!("PostgreSQL connection failed: {error}"); } });
        db.batch_execute(include_str!("../migrations/001_games.sql")).await?;
        db.batch_execute(include_str!("../migrations/002_reconnect.sql")).await?;
        Ok(Self {nats,db:Mutex::new(db)})
    }

    pub async fn serve(self)->Result<()> {
        let mut creates=self.nats.subscribe(SUBJ_TOWN_CREATE).await?;
        let mut resumes=self.nats.subscribe(SUBJ_GAME_RESUME).await?;
        let mut quits=self.nats.subscribe(SUBJ_GAME_QUIT_USER).await?;
        let mut lists=self.nats.subscribe(SUBJ_TOWN_LIST).await?;
        let mut ends=self.nats.subscribe(SUBJ_GAME_END).await?;
        let mut touches=self.nats.subscribe(SUBJ_GAME_TOUCH).await?;
        let mut disconnects=self.nats.subscribe(SUBJ_GAME_DISCONNECT).await?;
        let this=Arc::new(self);
        tokio::spawn(async move {
            let mut reap=tokio::time::interval(Duration::from_secs(10));
            loop {
                let (message,response)=tokio::select! {
                    Some(message)=creates.next()=> {
                        let result=async {
                            this.expire_leases().await?;
                            let request:CreateTownRequest=serde_json::from_slice(&message.payload)?;
                            let town=this.create(request).await?;
                            this.response(town).await
                        }.await;
                        (message,Self::entry_response(result))
                    }
                    Some(message)=resumes.next()=> {
                        let result=async {
                            this.expire_leases().await?;
                            let request:ResumeGameRequest=serde_json::from_slice(&message.payload)?;
                            this.resume(&request.user.sub).await
                        }.await;
                        (message,Self::entry_response(result))
                    }
                    Some(message)=lists.next()=> {
                        let response=match this.list().await { Ok(towns)=>serde_json::to_vec(&towns).unwrap(), Err(error)=>{
                            tracing::warn!("town listing failed: {error}"); continue;
                        }};
                        (message,response)
                    }
                    Some(message)=ends.next()=> {
                        let result=async {
                            let request:EndGameRequest=serde_json::from_slice(&message.payload)?;
                            this.end_game(request).await
                        }.await;
                        (message,Self::operation_response(result))
                    }
                    Some(message)=quits.next()=> {
                        let result=async {
                            let request:ResumeGameRequest=serde_json::from_slice(&message.payload)?;
                            let rows=this.db.lock().await.query("SELECT player_id FROM twz_games WHERE auth0_sub=$1 AND ended_at IS NULL",&[&request.user.sub]).await?;
                            for row in rows {
                                this.end_game(EndGameRequest {player_id:row.get::<_,String>(0).parse()?,connection_id:None,reason:GameEndReason::IntentionalQuit}).await?;
                            }
                            Ok::<_,anyhow::Error>(())
                        }.await;
                        (message,Self::operation_response(result))
                    }
                    Some(message)=touches.next()=> {
                        let result=async { let request:GameConnection=serde_json::from_slice(&message.payload)?; this.connection_update(request,false).await }.await;
                        (message,Self::operation_response(result))
                    }
                    Some(message)=disconnects.next()=> {
                        let result=async { let request:GameConnection=serde_json::from_slice(&message.payload)?; this.connection_update(request,true).await }.await;
                        (message,Self::operation_response(result))
                    }
                    _=reap.tick()=> { if let Err(error)=this.expire_leases().await { tracing::warn!("game expiry failed: {error}"); } continue; }
                };
                if let Some(reply)=message.reply { let _=this.nats.publish(reply,response.into()).await; }
            }
        });
        Ok(())
    }

    fn entry_response(result:Result<CreateTownResponse>)->Vec<u8> {
        let response=result.unwrap_or_else(|error| {
            tracing::warn!("game entry failed: {error:#}");
            CreateTownResponse {ok:false,error:Some(error.to_string()),town:None,spawn_tile:None,connection_id:None,player_state:None}
        });
        serde_json::to_vec(&response).unwrap()
    }

    fn operation_response(result:Result<()>)->Vec<u8> {
        match result {
            Ok(())=>br#"{"ok":true}"#.to_vec(),
            Err(error)=> { tracing::warn!("game operation failed: {error:#}"); serde_json::json!({"ok":false,"error":error.to_string()}).to_string().into_bytes() }
        }
    }

    async fn create(&self,request:CreateTownRequest)->Result<Town> {
        let name=validate_town_name(&request.desired_name).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(!request.user.sub.is_empty(),"Missing authenticated user.");
        let mut db=self.db.lock().await;
        let tx=db.transaction().await?;
        tx.execute("SELECT pg_advisory_xact_lock(7489221)",&[]).await?;
        anyhow::ensure!(tx.query_opt("SELECT 1 FROM twz_games WHERE auth0_sub=$1 AND ended_at IS NULL",&[&request.user.sub]).await?.is_none(),
            "You already have a town. Reconnect to it or intentionally leave it before starting another game.");
        tx.execute("INSERT INTO twz_users(auth0_sub,display_name,email) VALUES($1,$2,$3) ON CONFLICT(auth0_sub) DO UPDATE SET display_name=EXCLUDED.display_name,email=EXCLUDED.email,last_seen_at=NOW()",
            &[&request.user.sub,&request.user.name,&request.user.email]).await?;
        let slot:i32=tx.query_one("SELECT n::integer FROM generate_series(0,(SELECT COUNT(*) FROM twz_games WHERE ended_at IS NULL)::integer) AS n WHERE NOT EXISTS (SELECT 1 FROM twz_games WHERE hall_slot=n AND ended_at IS NULL) ORDER BY n LIMIT 1",&[]).await?.get(0);
        let previous:i64=tx.query_one("SELECT COALESCE(MAX((town_json->>'created_at_ms')::bigint),0) FROM twz_games WHERE hall_slot=$1",&[&slot]).await?.get(0);
        // A per-site hybrid timestamp orders claims even if two allocations
        // occur in one clock millisecond, or the wall clock moves backwards.
        let created=(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64).max(previous.saturating_add(1));
        let town=Town {id:format!("t-{}",request.player_id),name,hall:hall_for_slot(slot),
            created_at_ms:created,mayor_player_id:request.player_id};
        let connection=ClientId::new_v4().to_string();
        tx.execute("INSERT INTO twz_games(player_id,auth0_sub,town_id,town_name,hall_slot,town_json,connection_id,checkpoint_json) VALUES($1,$2,$3,$4,$5,$6::text::jsonb,$7,$8::text::jsonb)",
            &[&request.player_id.to_string(),&request.user.sub,&town.id,&town.name,&slot,&serde_json::to_string(&town)?,&connection,&serde_json::to_string(&PlayerState::initial_for_town(&town))?]).await?;
        tx.commit().await?;
        Ok(town)
    }

    async fn response(&self,town:Town)->Result<CreateTownResponse> {
        let row=self.db.lock().await.query_one("SELECT connection_id,checkpoint_json::text FROM twz_games WHERE player_id=$1 AND ended_at IS NULL",&[&town.mayor_player_id.to_string()]).await?;
        let checkpoint=row.get::<_,Option<String>>(1).map(|text|serde_json::from_str(&text)).transpose()?.unwrap_or_else(||PlayerState::initial_for_town(&town));
        Ok(CreateTownResponse {ok:true,error:None,spawn_tile:Some(spawn_from_hall(town.hall.origin)),
            connection_id:row.get::<_,Option<String>>(0).map(|id|id.parse()).transpose()?,player_state:Some(checkpoint),town:Some(town)})
    }

    async fn resume(&self,sub:&str)->Result<CreateTownResponse> {
        let mut db=self.db.lock().await;
        let tx=db.transaction().await?;
        tx.execute("SELECT pg_advisory_xact_lock(7489221)",&[]).await?;
        let row=tx.query_opt("SELECT player_id,town_json::text,connection_id FROM twz_games WHERE auth0_sub=$1 AND ended_at IS NULL AND (disconnected_at IS NULL OR disconnected_at > NOW()-($2::bigint * INTERVAL '1 second')) ORDER BY started_at DESC LIMIT 1 FOR UPDATE",&[&sub,&(RECONNECT_GRACE_SECONDS as i64)]).await?;
        let Some(row)=row else {return Ok(CreateTownResponse {ok:true,error:None,town:None,spawn_tile:None,connection_id:None,player_state:None});};
        anyhow::ensure!(row.get::<_,Option<String>>(2).is_none(),"Your game is still connected elsewhere, or its connection is still closing. Try reconnecting shortly.");
        let pid:String=row.get(0);
        let town:Town=serde_json::from_str(&row.get::<_,String>(1))?;
        tx.execute("UPDATE twz_games SET connection_id=$2,disconnected_at=NULL,last_seen_at=NOW() WHERE player_id=$1",&[&pid,&ClientId::new_v4().to_string()]).await?;
        tx.commit().await?;
        drop(db);
        self.response(town).await
    }

    async fn connection_update(&self,request:GameConnection,disconnect:bool)->Result<()> {
        if let Some(state)=&request.checkpoint {
            anyhow::ensure!(state.id==request.player_id && state.game.identity.game_id==request.player_id,
                "Checkpoint identity does not match the game.");
            anyhow::ensure!([state.x,state.y,state.vx,state.vy,state.ang].iter().all(|v|v.is_finite()),"Invalid checkpoint coordinates.");
        }
        let checkpoint=request.checkpoint.as_ref().map(serde_json::to_string).transpose()?;
        let sql=if disconnect {
            "UPDATE twz_games SET disconnected_at=NOW(),connection_id=NULL,last_seen_at=NOW(),checkpoint_json=COALESCE($3::text::jsonb,checkpoint_json) WHERE player_id=$1 AND connection_id=$2 AND ended_at IS NULL"
        } else {
            "UPDATE twz_games SET last_seen_at=NOW(),checkpoint_json=COALESCE($3::text::jsonb,checkpoint_json) WHERE player_id=$1 AND connection_id=$2 AND ended_at IS NULL"
        };
        let changed=self.db.lock().await.execute(sql,&[&request.player_id.to_string(),&request.connection_id.to_string(),&checkpoint]).await?;
        anyhow::ensure!(changed==1,"This game connection is no longer active.");
        Ok(())
    }

    async fn list(&self)->Result<Vec<Town>> {
        self.db.lock().await.query("SELECT town_json::text FROM twz_games WHERE ended_at IS NULL ORDER BY started_at",&[]).await?
            .into_iter().map(|row|Ok(serde_json::from_str(&row.get::<_,String>(0))?)).collect()
    }

    async fn end_game(&self,request:EndGameRequest)->Result<()> {
        let connection=request.connection_id.map(|id|id.to_string());
        let reason=serde_json::to_value(request.reason)?.as_str().unwrap().to_owned();
        let row=self.db.lock().await.query_opt("UPDATE twz_games SET ended_at=NOW(),end_reason=$3,connection_id=NULL WHERE player_id=$1 AND ended_at IS NULL AND ($2::text IS NULL OR connection_id=$2) RETURNING town_json::text",
            &[&request.player_id.to_string(),&connection,&reason]).await?;
        if let Some(row)=row {
            let town:Town=serde_json::from_str(&row.get::<_,String>(0))?;
            // Promotion selection is deferred. There are no successor roles in
            // today's playable roster, so the no-successor outcome releases it.
            if matches!(request.reason,GameEndReason::IntentionalQuit|GameEndReason::Timeout) {
                self.nats.publish(SUBJ_PROMOTION_REQUESTED,serde_json::json!({"town":town,"former_mayor_id":request.player_id,"reason":request.reason,"eligible_successors":[],"outcome":"no_successors"}).to_string().into()).await?;
            }
            self.nats.publish(SUBJ_GAME_ENDED,serde_json::to_vec(&GameEnded {player_id:request.player_id,reason:request.reason})?.into()).await?;
        }
        Ok(())
    }

    async fn expire_leases(&self)->Result<u64> {
        let db=self.db.lock().await;
        // A crashed gateway has no final disconnect message. Its last heartbeat
        // starts the same grace period; stale connection tokens cannot renew it.
        db.execute("UPDATE twz_games SET disconnected_at=last_seen_at,connection_id=NULL WHERE ended_at IS NULL AND disconnected_at IS NULL AND last_seen_at < NOW()-INTERVAL '45 seconds'",&[]).await?;
        let rows=db.query("SELECT player_id FROM twz_games WHERE ended_at IS NULL AND disconnected_at < NOW()-($1::bigint * INTERVAL '1 second')",&[&(RECONNECT_GRACE_SECONDS as i64)]).await?;
        drop(db);
        let count=rows.len() as u64;
        for row in rows {self.end_game(EndGameRequest {player_id:row.get::<_,String>(0).parse()?,connection_id:None,reason:GameEndReason::Timeout}).await?;}
        Ok(count)
    }
}

#[cfg(test)]
mod tests;
