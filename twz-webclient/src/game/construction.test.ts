import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ConstructionCommands } from './constructionCommands';
import { catalogEndpoint, catalogLock, isBuildingCatalog, isEconomyState, isConstructionReply, previewPlacement,
  refundFor, screenToTile, type BuildingCatalog, type BuildingDefinition, type ConstructionCommand, type ConstructionReply,
  type EconomyState, type OwnedBuilding } from './construction';
import { isSnapshot, type Snapshot } from './protocol';

const gameId = '11111111-1111-4111-8111-111111111111';
const research: BuildingDefinition = { building_type: 'wall_research', name: 'Wall Research', cost: 5000, prerequisite: null, status: 'specified',
  geometry: { footprint_tiles: { x: 3, y: 3 }, collision_px: { x: 0, y: 0, width: 144, height: 144 }, visual_bounds_px: null } };
const factory: BuildingDefinition = { ...research, building_type: 'wall_factory', name: 'Wall Factory', cost: 50000, prerequisite: 'wall_research',
  geometry: { ...research.geometry, collision_px: { ...research.geometry.collision_px, height: 96 } } };
const catalog: BuildingCatalog = { version: 2, buildings: [research, factory] };
const ownedResearch: OwnedBuilding = { building: { id: 'research-1', building_type: 'wall_research', origin_tile: { x: 2, y: 2 },
  ownership: { game_id: gameId, town_id: 'town', owner_player_id: gameId }, health: { current: 255, maximum: 255 }, production: null }, price_paid: 5003 };
function fixture(): { snapshot: Snapshot; economy: EconomyState } {
  return {
    snapshot: { type: 'state', t: 1, me: gameId, players: [[gameId, 816, 912, 0, 100]], bullets: [], catalog_version: 2, buildings: [],
      tiles: { ox: 0, oy: 0, w: 32, h: 32, data: Array(1024).fill(0) },
      construction: { territory: { min_tile: { x: 0, y: 0 }, size_tiles: { x: 32, y: 32 } }, can_construct: true, reason: null } },
    economy: { type: 'economy_state', cash: 55000, revision: 4, territory: { min_tile: { x: 0, y: 0 }, size_tiles: { x: 32, y: 32 } },
      owned_buildings: [], constructible_types: ['wall_research', 'wall_factory'], demolition_refund_percent: 25 },
  };
}
const success = (command: ConstructionCommand, outcome_known = true): ConstructionReply => ({ type: 'construction_result', outcome_known,
  result: { request_id: command.request_id, error: null, building_id: 'building-1', cash: 0, refund: 0 } });
function storage() {
  const entries = new Map<string, string>();
  return { getItem: (key: string) => entries.get(key) ?? null, setItem: (key: string, value: string) => { entries.set(key, value); }, removeItem: (key: string) => { entries.delete(key); } };
}

test('catalog and economy boundaries reject malformed authority data', () => {
  assert.equal(isBuildingCatalog(catalog), true);
  assert.equal(isBuildingCatalog({ ...catalog, buildings: [research, research, factory] }), false);
  assert.equal(isBuildingCatalog({ ...catalog, buildings: [{ ...research, cost: -1 }, factory] }), false);
  const { snapshot, economy } = fixture();
  economy.owned_buildings = [ownedResearch];
  assert.equal(isEconomyState(economy), true);
  assert.equal(isEconomyState({ ...economy, demolition_refund_percent: 101 }), false);
  assert.equal(isEconomyState({ ...economy, owned_buildings: [ownedResearch, ownedResearch] }), false);
  assert.equal(isEconomyState({ ...economy, cash: Number.MAX_SAFE_INTEGER + 1 }), false);
  assert.equal(isSnapshot(snapshot), true);
  assert.equal(isSnapshot({ ...snapshot, construction: { can_construct: 'yes' } }), false);
  const command: ConstructionCommand = { type: 'demolish_building', request_id: 'request-1', building_id: 'building-1' };
  assert.equal(isConstructionReply(success(command)), true);
  assert.equal(isConstructionReply({ type: 'construction_result', result: success(command).result }), false);
});

test('catalog URL follows gateway origin and reverse-proxy prefix without carrying query parameters', () => {
  assert.equal(catalogEndpoint('ws://localhost:8081/play'), 'http://localhost:8081/catalog');
  assert.equal(catalogEndpoint('wss://example.com/game/play?ignored=value'), 'https://example.com/game/catalog');
});

test('factory locks use town-wide research and server capability rather than nearby snapshots', () => {
  const { snapshot, economy } = fixture();
  assert.match(catalogLock(factory, economy, snapshot, catalog)!, /Requires Wall Research/);
  economy.owned_buildings = [ownedResearch];
  assert.equal(catalogLock(factory, economy, snapshot, catalog), null);
  economy.cash = 49999;
  assert.match(catalogLock(factory, economy, snapshot, catalog)!, /Need 1 more cash/);
  economy.cash = 55000; economy.constructible_types = ['wall_research'];
  assert.match(catalogLock(factory, economy, snapshot, catalog)!, /later phase/);
  assert.match(catalogLock(research, economy, { ...snapshot, catalog_version: 3 }, catalog)!, /version changed/);
});

test('placement preview reserves full footprints and distinguishes unknown terrain from valid ground', () => {
  const { snapshot, economy } = fixture();
  const anchor = { x: 13, y: 16 };
  assert.equal(previewPlacement(research, anchor, snapshot, economy, catalog).state, 'valid');
  assert.match(previewPlacement(research, { x: 30, y: 16 }, snapshot, economy, catalog).message, /entire footprint/);
  snapshot.tiles!.data[16 * 32 + 13] = 1;
  assert.match(previewPlacement(research, anchor, snapshot, economy, catalog).message, /ground/);
  snapshot.tiles!.data[16 * 32 + 13] = 0;
  assert.equal(previewPlacement(research, anchor, { ...snapshot, tiles: undefined }, economy, catalog).state, 'unknown');
  economy.owned_buildings = [{ ...ownedResearch, building: { ...ownedResearch.building, origin_tile: anchor } }];
  assert.match(previewPlacement(research, { x: 13, y: 18 }, snapshot, economy, catalog).message, /forecourts/);
});

test('living player obstruction checks the collider, not the walkable forecourt', () => {
  const { snapshot, economy } = fixture();
  economy.owned_buildings = [ownedResearch];
  snapshot.players.push(['other', 14 * 48, 18.5 * 48, 0, 100]);
  const anchor = { x: 13, y: 16 };
  assert.equal(previewPlacement(factory, anchor, snapshot, economy, catalog).state, 'valid');
  snapshot.players[1][2] = 17.5 * 48;
  assert.match(previewPlacement(factory, anchor, snapshot, economy, catalog).message, /living player/);
  snapshot.players[1][4] = 0;
  assert.equal(previewPlacement(factory, anchor, snapshot, economy, catalog).state, 'valid');
});

test('grid anchors floor negative positions and refunds use integer rounding without precision loss', () => {
  assert.deepEqual(screenToTile(576, 384, 1152, 768, -1, -1), { x: -1, y: -1 });
  assert.deepEqual(screenToTile(576 - 49, 384, 1152, 768, 816, 912), { x: 15, y: 19 });
  assert.equal(refundFor(5003, 25), 1250);
  assert.equal(refundFor(Number.MAX_SAFE_INTEGER, 25), Number(BigInt(Number.MAX_SAFE_INTEGER) / 4n));
});

test('uncertain command retries preserve the exact request ID/payload and ignore unrelated results', () => {
  const sent: ConstructionCommand[] = [];
  const channel = new ConstructionCommands(() => {}, storage());
  channel.setSession(gameId, command => { sent.push(command); return true; });
  const origin = { x: 13, y: 16 };
  assert.equal(channel.submit({ type: 'place_building', building_type: 'wall_research', origin_tile: origin }), true);
  origin.x = 999;
  assert.equal(channel.submit({ type: 'demolish_building', building_id: 'other' }), false);
  assert.equal(channel.receive({ ...success(sent[0]), result: { ...success(sent[0]).result, request_id: 'unrelated' } }), false);
  channel.receive({ ...success(sent[0], false), result: { ...success(sent[0]).result, error: 'service_unavailable', building_id: null } });
  assert.equal(channel.state.pending?.phase, 'uncertain');
  assert.equal(channel.retry(), true);
  assert.deepEqual(sent[1], sent[0]);
  assert.equal(sent[1].type === 'place_building' && sent[1].origin_tile.x, 13);
  channel.receive(success(sent[0]));
  assert.equal(channel.state.pending, null);
  assert.equal(channel.state.notice?.kind, 'success');
  channel.dispose();
});

test('reload/reconnect restores unresolved requests only for the same participation', () => {
  const saved = storage();
  const sent: ConstructionCommand[] = [];
  const first = new ConstructionCommands(() => {}, saved);
  first.setSession(gameId, command => { sent.push(command); return true; });
  first.submit({ type: 'demolish_building', building_id: 'building-1' }); first.disconnect(); first.dispose();
  const resumed = new ConstructionCommands(() => {}, saved);
  resumed.setSession(gameId, command => { sent.push(command); return true; });
  assert.equal(resumed.state.pending?.phase, 'uncertain');
  resumed.retry(); assert.deepEqual(sent[1], sent[0]);
  resumed.setSession('different-game', command => { sent.push(command); return true; });
  assert.equal(resumed.state.pending, null);
  assert.equal(resumed.receive(success(sent[0])), false);
  resumed.dispose();
});

test('timeouts retain the intention and recorded rejections allow a new request', async () => {
  const sent: ConstructionCommand[] = [];
  const channel = new ConstructionCommands(() => {}, storage(), 10);
  channel.setSession(gameId, command => { sent.push(command); return true; });
  channel.submit({ type: 'demolish_building', building_id: 'building-1' });
  await new Promise(resolve => setTimeout(resolve, 25));
  assert.equal(channel.state.pending?.phase, 'uncertain');
  channel.receive({ ...success(sent[0]), result: { ...success(sent[0]).result, error: 'not_owned', building_id: null } });
  assert.equal(channel.state.pending, null);
  assert.equal(channel.state.notice?.kind, 'error');
  channel.submit({ type: 'demolish_building', building_id: 'building-1' });
  assert.notEqual(sent[1].request_id, sent[0].request_id);
  channel.endSession(); channel.dispose();
});
