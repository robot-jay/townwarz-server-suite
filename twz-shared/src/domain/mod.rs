//! Versioned gameplay contracts. Definitions describe content; instances hold
//! authoritative mutable state. Defining content does not enable construction.
mod catalog;
mod combat;
mod entities;
mod player;

pub use catalog::*;
pub use combat::*;
pub use entities::*;
pub use player::*;

pub type TownId = String;
pub type GameId = uuid::Uuid;
pub type EntityId = uuid::Uuid;

pub const DOMAIN_VERSION: u16 = 1;
pub const INVENTORY_SLOTS: usize = 6;
