use serde::{Deserialize, Serialize};
use crate::ClientId;
use super::{EntityId, GameId, TownId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Combatant {
    pub player_id: ClientId,
    pub game_id: GameId,
    pub town_id: Option<TownId>,
}

impl Combatant {
    pub fn is_friendly_to(&self, other: &Self) -> bool {
        (self.player_id == other.player_id && self.game_id == other.game_id)
            || matches!((&self.town_id, &other.town_id), (Some(a), Some(b)) if a == b)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DamageSourceKind { Player, Turret, HomingMissile, Orb, Environment }

/// Capture at fire/deploy time. No lookup of a live shooter is required when
/// the hit happens, so disconnect/handoff cannot alter allegiance or kill credit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DamageAttribution {
    pub source_entity_id: EntityId,
    pub source_kind: DamageSourceKind,
    pub beneficiary: Option<Combatant>,
}

impl DamageAttribution {
    pub fn can_damage(&self, target: &Combatant) -> bool {
        !self.beneficiary.as_ref().is_some_and(|owner| owner.is_friendly_to(target))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewardTargetKind { Player, Building, ItemBuilding }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DamageContribution {
    pub credit: DamageAttribution,
    pub damage: u32,
    pub last_hit_at_ms: u64,
}

/// Future rewards service input. Event ID allows an eventual ledger to apply
/// credit once; contributions permit assist rules without changing projectiles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefeatRecord {
    pub event_id: EntityId,
    pub target_id: EntityId,
    pub target_game_id: GameId,
    pub target_kind: RewardTargetKind,
    pub credit: DamageAttribution,
    pub contributions: Vec<DamageContribution>,
    pub occurred_at_ms: u64,
}

/// None means balance/design is undecided. There are no payouts in Phase 1.
/// A future reward distributor credits player wallets, including eligible town
/// members on other cells; it does not turn team cash into a shared treasury.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewardPolicy {
    pub team_member_share: Option<CashShare>,
    pub assist_window_ms: Option<u64>,
}

/// Percentage in basis points, with integer arithmetic and an explicit policy
/// value instead of hard-coding an unapproved team/assist percentage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct CashShare(u16);

impl TryFrom<u16> for CashShare {
    type Error = &'static str;
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        if value <= 10_000 { Ok(Self(value)) } else { Err("cash share must be between 0 and 10000 basis points") }
    }
}

impl From<CashShare> for u16 {
    fn from(value: CashShare) -> Self { value.0 }
}

impl CashShare {
    pub fn amount(self, base_cash: u64) -> u64 {
        ((base_cash as u128 * self.0 as u128) / 10_000) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn allegiance_is_by_town_and_anonymous_players_are_independent() {
        let id = Uuid::new_v4();
        let a = Combatant { player_id: id, game_id: id, town_id: Some("a".into()) };
        let mut b = a.clone();
        b.player_id = Uuid::new_v4();
        b.game_id = Uuid::new_v4();
        assert!(a.is_friendly_to(&b));
        b.town_id = Some("b".into());
        assert!(!a.is_friendly_to(&b));
        b.town_id = None;
        let mut debug = a.clone();
        debug.player_id = Uuid::new_v4();
        debug.game_id = Uuid::new_v4();
        debug.town_id = None;
        assert!(!debug.is_friendly_to(&b));
        assert!(debug.is_friendly_to(&debug));
        assert!(!a.is_friendly_to(&debug));
    }

    #[test]
    fn reward_percentage_is_checked_and_rounds_down_without_overflow() {
        let share = CashShare::try_from(1250).unwrap();
        assert_eq!(share.amount(101), 12);
        assert_eq!(CashShare::try_from(10_000).unwrap().amount(u64::MAX), u64::MAX);
        assert!(CashShare::try_from(10_001).is_err());
        assert!(serde_json::from_str::<CashShare>("10001").is_err());
        assert_eq!(RewardPolicy::default().team_member_share, None);
    }
}
