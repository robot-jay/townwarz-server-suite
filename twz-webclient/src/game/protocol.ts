import { isBuildingView, isPlayerAffiliation, isPlayerGameState, type BuildingView, type PlayerAffiliation, type PlayerGameState } from './domain';

export type Player = [id: string, x: number, y: number, angle: number, hp: number];
export interface Bullet { id: number; x: number; y: number; vx: number; vy: number; owner: string; born_ms: number }
export interface TileWindow { ox: number; oy: number; w: number; h: number; data: number[] }
export interface Town {
  id: string; name: string; mayor_player_id: string;
  hall: { id: string; cell: [number, number]; origin: { x: number; y: number } };
}
export interface Snapshot {
  type: 'state'; t: number; me: string; players: Player[]; bullets: Bullet[]; tiles?: TileWindow; towns?: Town[];
  catalog_version?: number; self_state?: PlayerGameState; player_affiliations?: PlayerAffiliation[]; buildings?: BuildingView[];
}
export interface Input { up: boolean; down: boolean; left: boolean; right: boolean; shoot: boolean }
export const emptyInput = (): Input => ({ up: false, down: false, left: false, right: false, shoot: false });

export function isSnapshot(value: unknown): value is Snapshot {
  if (!value || typeof value !== 'object') return false;
  const s = value as Snapshot;
  return s.type === 'state' && Number.isFinite(s.t) && typeof s.me === 'string'
    && Array.isArray(s.players) && s.players.every(p => Array.isArray(p) && typeof p[0] === 'string'
      && p.length >= 5 && p.slice(1, 5).every(n => typeof n === 'number' && Number.isFinite(n)))
    && Array.isArray(s.bullets) && s.bullets.every(b => b && Number.isFinite(b.x) && Number.isFinite(b.y))
    && (s.buildings === undefined || (Array.isArray(s.buildings) && s.buildings.every(isBuildingView)))
    && (s.catalog_version === undefined || (Number.isSafeInteger(s.catalog_version) && s.catalog_version > 0))
    && (s.self_state === undefined || isPlayerGameState(s.self_state))
    && (s.player_affiliations === undefined || (Array.isArray(s.player_affiliations) && s.player_affiliations.every(isPlayerAffiliation)))
    && (!s.towns || (Array.isArray(s.towns) && s.towns.every(town => town && typeof town.id === 'string'
      && typeof town.name === 'string' && typeof town.mayor_player_id === 'string'
      && town.hall && town.hall.origin && Number.isInteger(town.hall.origin.x) && Number.isInteger(town.hall.origin.y))))
    && (!s.tiles || (Number.isInteger(s.tiles.ox) && Number.isInteger(s.tiles.oy)
      && Number.isInteger(s.tiles.w) && Number.isInteger(s.tiles.h)
      && s.tiles.w > 0 && s.tiles.h > 0 && s.tiles.w <= 256 && s.tiles.h <= 256
      && Array.isArray(s.tiles.data) && s.tiles.data.length === s.tiles.w * s.tiles.h));
}
