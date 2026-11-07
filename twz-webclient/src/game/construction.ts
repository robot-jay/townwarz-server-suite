import { BUILDING_TYPES, type BuildingType, type BuildingView, type PixelRect } from './domain';
import type { Snapshot } from './protocol';

export const TILE_SIZE = 48;
export const CONSTRUCTION_TYPES = ['wall_research', 'wall_factory'] as const;
export type ConstructibleType = typeof CONSTRUCTION_TYPES[number];
export interface TilePoint { x: number; y: number }
export interface Territory { min_tile: TilePoint; size_tiles: TilePoint }
export type Geometry = BuildingView['geometry'];
export interface BuildingDefinition {
  building_type: BuildingType; name: string; cost: number; prerequisite: BuildingType | null;
  status: 'specified' | 'unavailable'; geometry: Geometry;
}
export interface BuildingCatalog { version: number; buildings: BuildingDefinition[] }
export interface OwnedBuilding {
  building: Pick<BuildingView, 'id' | 'building_type' | 'origin_tile' | 'ownership' | 'health' | 'production'>;
  price_paid: number;
}
export interface EconomyState {
  type: 'economy_state'; cash: number; revision: number; territory: Territory;
  owned_buildings: OwnedBuilding[]; constructible_types: BuildingType[]; demolition_refund_percent: number;
}
export interface ConstructionStatus { territory: Territory | null; can_construct: boolean; reason: string | null }
export type ConstructionCommand =
  | { type: 'place_building'; request_id: string; building_type: ConstructibleType; origin_tile: TilePoint }
  | { type: 'demolish_building'; request_id: string; building_id: string };
export type ConstructionIntent =
  | { type: 'place_building'; building_type: ConstructibleType; origin_tile: TilePoint }
  | { type: 'demolish_building'; building_id: string };
export interface ConstructionResult { request_id: string; error: string | null; building_id: string | null; cash: number; refund: number }
export interface ConstructionReply { type: 'construction_result'; result: ConstructionResult; outcome_known: boolean }
export interface Preview { state: 'valid' | 'invalid' | 'unknown'; message: string }

const record = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v);
const natural = (v: unknown): v is number => typeof v === 'number' && Number.isSafeInteger(v) && v >= 0;
const id = (v: unknown): v is string => typeof v === 'string' && v.length > 0 && v.length <= 128;
const buildingType = (v: unknown): v is BuildingType => BUILDING_TYPES.includes(v as BuildingType);
export const isTilePoint = (v: unknown): v is TilePoint => record(v) && Number.isSafeInteger(v.x) && Number.isSafeInteger(v.y)
  && Math.abs(v.x as number) <= 2147483647 && Math.abs(v.y as number) <= 2147483647;
export function isTerritory(v: unknown): v is Territory {
  return record(v) && isTilePoint(v.min_tile) && isTilePoint(v.size_tiles)
    && v.size_tiles.x > 0 && v.size_tiles.x <= 64 && v.size_tiles.y > 0 && v.size_tiles.y <= 64;
}
function isGeometry(v: unknown): v is Geometry {
  const rect = (r: unknown): r is PixelRect => record(r) && natural(r.x) && natural(r.y)
    && natural(r.width) && r.width > 0 && r.width <= 4096 && natural(r.height) && r.height > 0 && r.height <= 4096;
  if (!record(v) || !isTilePoint(v.footprint_tiles) || v.footprint_tiles.x < 1 || v.footprint_tiles.y < 1
    || v.footprint_tiles.x > 64 || v.footprint_tiles.y > 64 || !rect(v.collision_px)) return false;
  return v.collision_px.x + v.collision_px.width <= v.footprint_tiles.x * TILE_SIZE
    && v.collision_px.y + v.collision_px.height <= v.footprint_tiles.y * TILE_SIZE
    && (v.visual_bounds_px === null || rect(v.visual_bounds_px));
}
export function isBuildingCatalog(v: unknown): v is BuildingCatalog {
  if (!record(v) || !natural(v.version) || v.version < 1 || !Array.isArray(v.buildings) || v.buildings.length > 100) return false;
  const buildings = v.buildings;
  return buildings.every(b => record(b) && buildingType(b.building_type) && id(b.name) && natural(b.cost)
    && (b.prerequisite === null || buildingType(b.prerequisite)) && ['specified', 'unavailable'].includes(b.status as string)
    && isGeometry(b.geometry)) && new Set(buildings.map(b => b.building_type)).size === buildings.length
    && CONSTRUCTION_TYPES.every(type => buildings.some(b => b.building_type === type));
}
export function isEconomyState(v: unknown): v is EconomyState {
  if (!record(v) || v.type !== 'economy_state' || !natural(v.cash) || !natural(v.revision) || !isTerritory(v.territory)
    || !natural(v.demolition_refund_percent) || v.demolition_refund_percent > 100 || !Array.isArray(v.constructible_types)
    || !v.constructible_types.every(buildingType) || !Array.isArray(v.owned_buildings)) return false;
  return v.owned_buildings.every(r => record(r) && natural(r.price_paid) && record(r.building)
    && id(r.building.id) && buildingType(r.building.building_type) && isTilePoint(r.building.origin_tile)
    && record(r.building.health) && natural(r.building.health.current) && natural(r.building.health.maximum)
    && r.building.health.current <= r.building.health.maximum && 'production' in r.building
    && record(r.building.ownership) && id(r.building.ownership.game_id)
    && id(r.building.ownership.town_id) && id(r.building.ownership.owner_player_id))
    && new Set(v.owned_buildings.map(r => r.building.id)).size === v.owned_buildings.length;
}
export function isConstructionStatus(v: unknown): v is ConstructionStatus {
  return record(v) && (v.territory === null || isTerritory(v.territory)) && typeof v.can_construct === 'boolean'
    && (v.reason === null || typeof v.reason === 'string');
}
export function isConstructionReply(v: unknown): v is ConstructionReply {
  if (!record(v) || v.type !== 'construction_result' || typeof v.outcome_known !== 'boolean' || !record(v.result)) return false;
  const r = v.result;
  return id(r.request_id) && (r.error === null || typeof r.error === 'string') && (r.building_id === null || id(r.building_id))
    && natural(r.cash) && natural(r.refund) && (r.error !== null || r.building_id !== null);
}
export function isConstructionCommand(v: unknown): v is ConstructionCommand {
  if (!record(v) || !id(v.request_id)) return false;
  return v.type === 'place_building' ? CONSTRUCTION_TYPES.includes(v.building_type as ConstructibleType) && isTilePoint(v.origin_tile)
    : v.type === 'demolish_building' && id(v.building_id);
}

export const formatCash = (cash: number) => cash.toLocaleString('en-US');
export function refundFor(price: number, percent: number): number { return Number(BigInt(price) * BigInt(percent) / 100n); }
export function catalogEndpoint(gameUrl: string): string {
  const url = new URL(gameUrl);
  url.protocol = url.protocol === 'wss:' ? 'https:' : 'http:';
  url.pathname = url.pathname.replace(/\/[^/]*\/?$/, '/catalog');
  url.search = ''; url.hash = '';
  return url.toString();
}
export const rejectionMessage = (reason: string | null): string => {
  const messages: Record<string, string> = {
    inactive_game: 'This game is no longer active.', stale_connection: 'Reconnect before managing your town.',
    not_mayor: 'Only the Mayor can manage infrastructure.', not_alive: 'Wait until you respawn to build or demolish.',
    outside_territory: 'Return to your town territory to build or demolish.', unavailable_building: 'This infrastructure is not available yet.',
    insufficient_cash: 'You do not have enough cash.', missing_research: 'Build the required research first.',
    invalid_terrain: 'The entire footprint must be on ground.', occupied: 'That footprint is already occupied.',
    player_obstruction: 'A living player is in the solid building area.', not_owned: 'You can only demolish your own infrastructure.',
    town_hall_protected: 'Townhalls cannot be demolished.', request_id_conflict: 'This request ID was already used for a different action.',
    authority_unavailable: 'Your town server is unavailable. Try again once it recovers.', busy: 'The server is handling another action. Try again.',
    cash_limit: 'The refund would exceed the wallet limit.', service_unavailable: 'The economy service is unavailable.',
    invalid_command: 'The server could not read this construction request.',
  };
  return reason ? messages[reason] ?? `The server rejected this action (${reason.replaceAll('_', ' ')}).` : '';
};
export function footprintInside(territory: Territory, origin: TilePoint, size: TilePoint): boolean {
  return origin.x >= territory.min_tile.x && origin.y >= territory.min_tile.y
    && origin.x + size.x <= territory.min_tile.x + territory.size_tiles.x
    && origin.y + size.y <= territory.min_tile.y + territory.size_tiles.y;
}
export function catalogLock(definition: BuildingDefinition, economy: EconomyState | null, snapshot: Snapshot | null, catalog: BuildingCatalog): string | null {
  if (!economy || !snapshot?.construction) return 'Waiting for authoritative construction state…';
  if (catalog.version !== snapshot.catalog_version) return 'Catalog version changed. Refresh the catalog.';
  if (definition.status !== 'specified' || !economy.constructible_types.includes(definition.building_type)) return 'Available in a later phase.';
  if (!snapshot.construction.can_construct) return rejectionMessage(snapshot.construction.reason) || 'Construction is unavailable.';
  if (definition.prerequisite && !economy.owned_buildings.some(r => r.building.building_type === definition.prerequisite)) {
    return `Requires ${catalog.buildings.find(b => b.building_type === definition.prerequisite)?.name ?? 'research'}.`;
  }
  return economy.cash < definition.cost ? `Need ${formatCash(definition.cost - economy.cash)} more cash.` : null;
}
function overlaps(a: TilePoint, size: TilePoint, b: TilePoint, otherSize: TilePoint): boolean {
  return a.x < b.x + otherSize.x && a.x + size.x > b.x && a.y < b.y + otherSize.y && a.y + size.y > b.y;
}
export function previewPlacement(definition: BuildingDefinition, origin: TilePoint, snapshot: Snapshot, economy: EconomyState, catalog: BuildingCatalog): Preview {
  const invalid = (message: string): Preview => ({ state: 'invalid', message });
  const lock = catalogLock(definition, economy, snapshot, catalog);
  if (lock) return invalid(lock);
  const size = definition.geometry.footprint_tiles;
  if (!footprintInside(economy.territory, origin, size)) return invalid('The entire footprint must stay inside your town territory.');
  const buildings = [...(snapshot.buildings ?? [])];
  for (const record of economy.owned_buildings) {
    const d = catalog.buildings.find(b => b.building_type === record.building.building_type);
    if (d && !buildings.some(b => b.id === record.building.id)) buildings.push({ ...record.building, geometry: d.geometry, town_name: null });
  }
  for (const building of buildings) {
    if (overlaps(origin, size, building.origin_tile, building.geometry.footprint_tiles)) return invalid('Reserved building footprints cannot overlap, including forecourts.');
  }
  const collider = definition.geometry.collision_px;
  const left = origin.x * TILE_SIZE + collider.x, top = origin.y * TILE_SIZE + collider.y;
  for (const [, x, y, , hp] of snapshot.players) {
    const dx = x - Math.max(left, Math.min(x, left + collider.width));
    const dy = y - Math.max(top, Math.min(y, top + collider.height));
    // Advisory preview of the current server's 16px player collision radius.
    if (hp > 0 && dx * dx + dy * dy < 16 * 16) return invalid('A living player is in the solid building area.');
  }
  const tiles = snapshot.tiles;
  let unknown = false;
  for (let y = origin.y; y < origin.y + size.y; y++) for (let x = origin.x; x < origin.x + size.x; x++) {
    if (!tiles || x < tiles.ox || y < tiles.oy || x >= tiles.ox + tiles.w || y >= tiles.oy + tiles.h) { unknown = true; continue; }
    if (tiles.data[(y - tiles.oy) * tiles.w + x - tiles.ox] !== 0) return invalid('The entire footprint must be on ground.');
  }
  return unknown ? { state: 'unknown', message: 'Some tiles are out of view; the server will verify this placement.' }
    : { state: 'valid', message: 'Footprint looks clear. The server makes the final placement check.' };
}
export function screenToTile(x: number, y: number, width: number, height: number, playerX: number, playerY: number): TilePoint {
  const scale = Math.min(width / (24 * TILE_SIZE), height / (16 * TILE_SIZE));
  return { x: Math.floor((playerX + (x - width / 2) / scale) / TILE_SIZE), y: Math.floor((playerY + (y - height / 2) / scale) / TILE_SIZE) };
}
