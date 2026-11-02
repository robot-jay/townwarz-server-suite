use serde::{Deserialize, Serialize};
use crate::ClientId;
use super::{Combatant, GameId, Inventory, TownId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerRole { Mayor, Commando }

/// Set by the gateway, never from gameplay input. The current one-player game
/// uses its fresh player UUID as game_id; the fields remain distinct so future
/// town members and reward recipients need not share a session identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameIdentity {
    pub game_id: GameId,
    pub town_id: Option<TownId>,
    /// None means an independent anonymous debug player, not a shared faction.
    pub role: Option<PlayerRole>,
}

impl GameIdentity {
    pub fn mayor(game_id: GameId, town_id: TownId) -> Self {
        Self { game_id, town_id: Some(town_id), role: Some(PlayerRole::Mayor) }
    }

    pub fn debug(game_id: GameId) -> Self {
        Self { game_id, town_id: None, role: None }
    }

    pub fn combatant(&self, player_id: ClientId) -> Combatant {
        Combatant { player_id, game_id: self.game_id, town_id: self.town_id.clone() }
    }
}

/// Cash is whole units, owned by an individual player. Keep JSON values exact
/// in the browser; all eventual spending/rewards must use checked operations.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wallet { pub cash: u64 }

impl Wallet {
    pub const MAX_CASH: u64 = 9_007_199_254_740_991;

    pub fn credit(&mut self, amount: u64) -> Result<(), &'static str> {
        let next = self.cash.checked_add(amount).filter(|value| *value <= Self::MAX_CASH)
            .ok_or("cash limit exceeded")?;
        self.cash = next;
        Ok(())
    }

    pub fn spend(&mut self, amount: u64) -> Result<(), &'static str> {
        self.cash = self.cash.checked_sub(amount).ok_or("insufficient cash")?;
        Ok(())
    }
}

/// Absolute server timestamps survive service handoffs. These are model fields
/// for future weapon/item cooldowns, not a change to today's firing balance.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerCooldowns {
    pub primary_fire_ready_at_ms: u64,
    pub item_use_ready_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerGameState {
    pub identity: GameIdentity,
    pub wallet: Wallet,
    pub inventory: Inventory,
    pub cooldowns: PlayerCooldowns,
}

impl PlayerGameState {
    pub fn new(identity: GameIdentity) -> Self {
        Self { identity, wallet: Wallet::default(), inventory: Inventory::default(), cooldowns: PlayerCooldowns::default() }
    }

    pub fn on_combat_death(&mut self) {
        self.inventory.clear();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerAffiliation {
    pub player_id: ClientId,
    pub town_id: Option<TownId>,
    pub role: Option<PlayerRole>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wallet_operations_are_atomic_and_do_not_overflow() {
        let mut wallet = Wallet { cash: 100 };
        assert!(wallet.spend(101).is_err());
        assert_eq!(wallet.cash, 100);
        wallet.spend(100).unwrap();
        wallet.credit(Wallet::MAX_CASH).unwrap();
        assert!(wallet.credit(1).is_err());
        assert_eq!(wallet.cash, Wallet::MAX_CASH);
    }
}
