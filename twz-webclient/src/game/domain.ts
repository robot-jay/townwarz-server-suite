export const DOMAIN_VERSION = 2;
export const INVENTORY_SLOTS = 6;

// Stable wire identifiers from twz-shared/domain/catalog.rs. Prices and recipes
// stay server-owned and are available through the gateway's GET /catalog.
export const ITEM_TYPES = [
  'cloak', 'rocket', 'med_kit', 'bomb', 'mine', 'orb', 'flare', 'dfg',
  'wall', 'turret', 'sleeper', 'plasma', 'homing_missile',
] as const;
export type ItemType = typeof ITEM_TYPES[number];
export type PlayerRole = 'mayor' | 'commando';

export interface ExistingGameSummary {
  player_id: string;
  town_id: string;
  town_name: string;
  is_connected: boolean;
  reconnect_until_ms: number | null;
}

export interface TownRoleVacancy {
  role: PlayerRole;
  capacity: number;
  occupied: number;
  vacancies: number;
  can_join: boolean;
  unavailable_reason: 'town_inactive' | 'role_full' | 'rejoining_unavailable' | null;
}

export interface ExpiredGameNotice {
  player_id: string;
  town_id: string;
  town_name: string;
  expired_at_ms: number;
  grace_period_seconds: number;
  former_town: {
    is_active: boolean;
    rejoin_supported: boolean;
    state_on_join: 'fresh';
    roles: TownRoleVacancy[];
  };
}

export const BUILDING_TYPES = [
  'town_hall', 'hospital', 'house', 'laser_research', 'laser_factory', 'turret_research', 'turret_factory',
  'time_bomb_research', 'time_bomb_factory', 'med_kit_research', 'plasma_turret_research', 'plasma_turret_factory',
  'mine_research', 'mine_factory', 'orb_research', 'orb_factory', 'bazooka_research', 'bazooka_factory',
  'sleeper_research', 'sleeper_factory', 'wall_research', 'wall_factory', 'dfg_research', 'dfg_factory',
  'flare_gun_research', 'flare_gun_factory', 'homing_missile_research', 'homing_missile_factory',
] as const;
export type BuildingType = typeof BUILDING_TYPES[number];
export interface PixelRect { x: number; y: number; width: number; height: number }
export interface BuildingView {
  id: string; building_type: BuildingType; ownership: EntityOwnership | null;
  origin_tile: { x: number; y: number }; health: { current: number; maximum: number }; production: unknown;
  geometry: { footprint_tiles: { x: number; y: number }; collision_px: PixelRect; visual_bounds_px: PixelRect | null };
  town_name: string | null;
}

export interface GameIdentity {
  game_id: string;
  town_id: string | null;
  role: PlayerRole | null;
}

export interface EntityOwnership {
  game_id: string;
  town_id: string | null;
  owner_player_id: string | null;
}

export interface ItemInstance {
  id: string;
  item_type: ItemType;
  ownership: EntityOwnership;
  source_building_id: string | null;
}

export interface PlayerGameState {
  identity: GameIdentity;
  wallet: { cash: number };
  inventory: { slots: (ItemInstance | null)[] };
  cooldowns: { primary_fire_ready_at_ms: number; item_use_ready_at_ms: number };
}

export interface PlayerAffiliation {
  player_id: string;
  town_id: string | null;
  role: PlayerRole | null;
}

const object = (value: unknown): value is Record<string, unknown> => Boolean(value) && typeof value === 'object';
const optionalId = (value: unknown) => value === null || typeof value === 'string';
const role = (value: unknown) => value === null || value === 'mayor' || value === 'commando';
const natural = (value: unknown) => typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;

export function isExistingGameSummary(value: unknown): value is ExistingGameSummary {
  return object(value) && typeof value.player_id === 'string' && value.player_id.length > 0
    && typeof value.town_id === 'string' && typeof value.town_name === 'string'
    && typeof value.is_connected === 'boolean'
    && (value.reconnect_until_ms === null || natural(value.reconnect_until_ms));
}

export function isExpiredGameNotice(value: unknown): value is ExpiredGameNotice {
  if (!object(value) || typeof value.player_id !== 'string' || !value.player_id
    || typeof value.town_id !== 'string' || typeof value.town_name !== 'string'
    || !natural(value.expired_at_ms) || !natural(value.grace_period_seconds) || value.grace_period_seconds === 0
    || !object(value.former_town)) return false;
  const town = value.former_town;
  return typeof town.is_active === 'boolean' && typeof town.rejoin_supported === 'boolean'
    && town.state_on_join === 'fresh' && Array.isArray(town.roles)
    && town.roles.every(slot => object(slot) && (slot.role === 'mayor' || slot.role === 'commando')
      && natural(slot.capacity) && (slot.capacity as number) > 0 && natural(slot.occupied) && natural(slot.vacancies)
      && slot.vacancies === Math.max(0, (slot.capacity as number) - (slot.occupied as number))
      && typeof slot.can_join === 'boolean'
      && (slot.unavailable_reason === null || ['town_inactive', 'role_full', 'rejoining_unavailable'].includes(slot.unavailable_reason as string))
      && (!slot.can_join || (town.is_active && town.rejoin_supported && (slot.vacancies as number) > 0)))
    && new Set(town.roles.map(slot => slot.role)).size === town.roles.length;
}

export function isPlayerGameState(value: unknown): value is PlayerGameState {
  if (!object(value) || !object(value.identity) || !object(value.wallet)
    || !object(value.inventory) || !object(value.cooldowns)) return false;
  const { identity, wallet, inventory, cooldowns } = value;
  return typeof identity.game_id === 'string' && optionalId(identity.town_id) && role(identity.role)
    && natural(wallet.cash)
    && natural(cooldowns.primary_fire_ready_at_ms) && natural(cooldowns.item_use_ready_at_ms)
    && Array.isArray(inventory.slots) && inventory.slots.length === INVENTORY_SLOTS
    && inventory.slots.every(item => item === null || (object(item)
      && typeof item.id === 'string' && ITEM_TYPES.includes(item.item_type as ItemType)
      && optionalId(item.source_building_id) && object(item.ownership)
      && typeof item.ownership.game_id === 'string' && optionalId(item.ownership.town_id)
      && optionalId(item.ownership.owner_player_id)));
}

export function isPlayerAffiliation(value: unknown): value is PlayerAffiliation {
  return object(value) && typeof value.player_id === 'string' && optionalId(value.town_id) && role(value.role);
}

export function isBuildingView(value: unknown): value is BuildingView {
  const rect = (v: unknown) => object(v) && natural(v.x) && natural(v.y)
    && natural(v.width) && (v.width as number) > 0 && (v.width as number) <= 4096
    && natural(v.height) && (v.height as number) > 0 && (v.height as number) <= 4096;
  if (!object(value) || typeof value.id !== 'string' || !BUILDING_TYPES.includes(value.building_type as BuildingType)
    || !object(value.origin_tile) || !Number.isSafeInteger(value.origin_tile.x) || !Number.isSafeInteger(value.origin_tile.y)
    || !object(value.health) || !natural(value.health.current) || !natural(value.health.maximum)
    || (value.health.current as number) > (value.health.maximum as number)
    || !object(value.geometry) || !object(value.geometry.footprint_tiles)) return false;
  const geometry = value.geometry;
  const footprint = geometry.footprint_tiles as Record<string, unknown>;
  return natural(footprint.x) && (footprint.x as number) > 0 && (footprint.x as number) <= 64
    && natural(footprint.y) && (footprint.y as number) > 0 && (footprint.y as number) <= 64
    && rect(geometry.collision_px) && (geometry.visual_bounds_px === null || rect(geometry.visual_bounds_px))
    && optionalId(value.town_name)
    && (value.ownership === null || (object(value.ownership) && typeof value.ownership.game_id === 'string'
      && optionalId(value.ownership.town_id) && optionalId(value.ownership.owner_player_id)));
}
