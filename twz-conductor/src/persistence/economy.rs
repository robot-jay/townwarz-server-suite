use super::{entities::games, GameRepository};
use anyhow::Result;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QuerySelect, Set,
    TransactionTrait,
};
use twz_shared::*;

/// Holds the game's database row lock across spatial preparation and durable
/// commit, but never holds a cell's world lock or runs IO in its simulation loop.
pub(crate) struct EconomyTransaction {
    transaction: sea_orm::DatabaseTransaction,
    model: games::Model,
    pub town: Town,
    pub state: EconomyState,
}
impl GameRepository {
    pub async fn begin_economy(
        &self,
        player_id: ClientId,
        connection_id: ClientId,
    ) -> Result<Result<EconomyTransaction, ConstructionError>> {
        let transaction = self.db.begin().await?;
        let row = games::Entity::find_by_id(player_id.to_string())
            .lock_exclusive()
            .one(&transaction)
            .await?;
        let Some(model) = row else {
            return Ok(Err(ConstructionError::InactiveGame));
        };
        if model.ended_at.is_some() {
            return Ok(Err(ConstructionError::InactiveGame));
        }
        if model.connection_id.as_deref() != Some(&connection_id.to_string())
            || model.disconnected_at.is_some()
        {
            return Ok(Err(ConstructionError::StaleConnection));
        }
        if model.role != "mayor" {
            return Ok(Err(ConstructionError::NotMayor));
        }
        let town = serde_json::from_value(model.town_json.clone())?;
        let state = match &model.economy_json {
            Some(value) => serde_json::from_value(value.clone())?,
            None => {
                let cash = model
                    .checkpoint_json
                    .clone()
                    .map(serde_json::from_value::<PlayerState>)
                    .transpose()?
                    .map(|p| p.game.wallet.cash)
                    .unwrap_or(0);
                EconomyState {
                    cash,
                    ..Default::default()
                }
            }
        };
        Ok(Ok(EconomyTransaction {
            transaction,
            model,
            town,
            state,
        }))
    }

    pub async fn economy_projections(&self) -> Result<Vec<EconomyProjection>> {
        let ids = games::Entity::find()
            .select_only()
            .column(games::Column::PlayerId)
            .filter(games::Column::EndedAt.is_null())
            .into_tuple::<String>()
            .all(&self.db)
            .await?;
        let mut projections = Vec::new();
        for id in ids {
            // Wait for any prepare/commit to resolve. An unlocked SELECT could
            // mistake the old committed version for an aborted reservation.
            let tx = self.db.begin().await?;
            if let Some(model) = games::Entity::find_by_id(id)
                .lock_exclusive()
                .one(&tx)
                .await?
            {
                if model.ended_at.is_none() {
                    let state = if let Some(value) = model.economy_json {
                        serde_json::from_value(value)?
                    } else {
                        EconomyState {
                            cash: model
                                .checkpoint_json
                                .map(serde_json::from_value::<PlayerState>)
                                .transpose()?
                                .map(|p| p.game.wallet.cash)
                                .unwrap_or(0),
                            ..Default::default()
                        }
                    };
                    projections.push(EconomyProjection::new(
                        serde_json::from_value(model.town_json)?,
                        &state,
                    ));
                }
            }
            tx.commit().await?;
        }
        Ok(projections)
    }
}
impl EconomyTransaction {
    pub fn projection(&self) -> EconomyProjection {
        EconomyProjection::new(self.town.clone(), &self.state)
    }
    pub async fn commit(mut self) -> Result<EconomyProjection> {
        self.state.revision = self
            .state
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Economy revision exhausted"))?;
        let projection = self.projection();
        let mut model = self.model.into_active_model();
        model.economy_json = Set(Some(serde_json::to_value(&self.state)?));
        model.update(&self.transaction).await?;
        self.transaction.commit().await?;
        Ok(projection)
    }
}
