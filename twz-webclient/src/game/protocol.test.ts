import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isSnapshot } from './protocol';
import { isPlayerGameState } from './domain';
import { isBuildingView } from './domain';
import { isExistingGameSummary } from './domain';
import { isExpiredGameNotice } from './domain';

const state = () => ({
  identity: { game_id: 'game-1', town_id: 'town-a', role: 'mayor' },
  wallet: { cash: 1250 }, inventory: { slots: [null, null, null, null, null, null] as unknown[] },
  cooldowns: { primary_fire_ready_at_ms: 123456, item_use_ready_at_ms: 123999 },
});

test('domain snapshot retains private inventory, wallet, identity and cooldowns', () => {
  const game = state();
  game.inventory.slots[2] = {
    id: 'item-1', item_type: 'homing_missile',
    ownership: { game_id: 'game-1', town_id: 'town-a', owner_player_id: 'player-1' }, source_building_id: 'factory-1',
  };
  assert.equal(isPlayerGameState(game), true);
  const snapshot = {
    type: 'state', t: 1, me: 'player-1', players: [['player-1', 816, 912, 0, 100]], bullets: [],
    catalog_version: 1, self_state: game, player_affiliations: [{ player_id: 'player-1', town_id: 'town-a', role: 'mayor' }],
  };
  assert.equal(isSnapshot(snapshot), true);
  assert.equal(isSnapshot({ ...snapshot, self_state: { ...game, wallet: { cash: -1 } } }), false);
  assert.equal(isSnapshot({ ...snapshot, player_affiliations: [{ player_id: 'p', town_id: null, role: 'hacker' }] }), false);
});

test('inventory rejects unsupported item identifiers and wrong slot counts', () => {
  const game = state();
  game.inventory.slots.pop();
  assert.equal(isPlayerGameState(game), false);
  game.inventory.slots.push({ id: 'item', item_type: 'unknown', ownership: { game_id: 'game', town_id: null, owner_player_id: null }, source_building_id: null });
  assert.equal(isPlayerGameState(game), false);
  assert.equal(isPlayerGameState({ ...state(), wallet: { cash: Number.MAX_SAFE_INTEGER + 1 } }), false);
});

test('anonymous identities and previous snapshots remain readable', () => {
  const game = state();
  assert.equal(isPlayerGameState({ ...game, identity: { game_id: 'debug-game', town_id: null, role: null } }), true);
  assert.equal(isSnapshot({ type: 'state', t: 1, me: 'p', players: [['p', 0, 0, 0, 100]], bullets: [] }), true);
});

test('physical halls have a three-tile footprint and a distinct forecourt', () => {
  const hall = {
    id: 'hall', building_type: 'town_hall', ownership: null, origin_tile: { x: -16, y: 16 },
    health: { current: 200, maximum: 200 }, production: null, town_name: null,
    geometry: { footprint_tiles: { x: 3, y: 3 }, collision_px: { x: 0, y: 0, width: 144, height: 96 },
      visual_bounds_px: { x: 0, y: 0, width: 144, height: 144 } },
  };
  assert.equal(isBuildingView(hall), true);
  assert.equal(hall.geometry.visual_bounds_px.height - hall.geometry.collision_px.height, 48);
  assert.equal(isBuildingView({ ...hall, geometry: { ...hall.geometry, collision_px: { ...hall.geometry.collision_px, height: -1 } } }), false);
  assert.equal(isBuildingView({ ...hall, building_type: 'made_up' }), false);
  assert.equal(isSnapshot({ type: 'state', t: 1, me: 'p', players: [['p', 0, 0, 0, 100]], bullets: [], buildings: [hall] }), true);
});

test('entry summaries identify the exact town to abandon without loading player state', () => {
  const summary = { player_id: 'game-a', town_id: 'town-a', town_name: 'Copper Creek', is_connected: true, reconnect_until_ms: null };
  assert.equal(isExistingGameSummary(summary), true);
  assert.equal(isExistingGameSummary({ ...summary, is_connected: false, reconnect_until_ms: Date.now() + 900000 }), true);
  assert.equal(isExistingGameSummary({ ...summary, player_id: '' }), false);
  assert.equal(isExistingGameSummary({ ...summary, is_connected: 'yes' }), false);
  assert.equal(isExistingGameSummary({ ...summary, reconnect_until_ms: NaN }), false);
});

test('expiry notices carry original town identity and explicit future role vacancies', () => {
  const notice = {
    player_id: 'expired-player', town_id: 'original-town', town_name: 'Old Town', expired_at_ms: Date.now(), grace_period_seconds: 900,
    former_town: { is_active: false, rejoin_supported: false, state_on_join: 'fresh', roles: [
      { role: 'mayor', capacity: 1, occupied: 0, vacancies: 1, can_join: false, unavailable_reason: 'town_inactive' },
      { role: 'commando', capacity: 4, occupied: 0, vacancies: 4, can_join: false, unavailable_reason: 'town_inactive' },
    ] },
  };
  assert.equal(isExpiredGameNotice(notice), true);
  assert.equal(isExpiredGameNotice({ ...notice, grace_period_seconds: 0 }), false);
  assert.equal(isExpiredGameNotice({ ...notice, former_town: { ...notice.former_town, roles: [{ ...notice.former_town.roles[0], can_join: true }] } }), false);
  assert.equal(isExpiredGameNotice({ ...notice, former_town: { ...notice.former_town, roles: [{ ...notice.former_town.roles[0], vacancies: 2 }] } }), false);
});
