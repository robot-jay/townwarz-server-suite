import { expect, test, type Page, type WebSocketRoute } from '@playwright/test';
import type { BuildingCatalog, ConstructionCommand, ConstructionResult, OwnedBuilding } from '../src/game/construction';

const id = '11111111-1111-4111-8111-111111111111';
const catalog: BuildingCatalog = { version: 2, buildings: [
  { building_type: 'town_hall', name: 'Town Hall', cost: 0, prerequisite: null, status: 'specified', geometry: { footprint_tiles: { x: 3, y: 3 }, collision_px: { x: 0, y: 0, width: 144, height: 96 }, visual_bounds_px: null } },
  { building_type: 'wall_research', name: 'Wall Research', cost: 5000, prerequisite: null, status: 'specified', geometry: { footprint_tiles: { x: 3, y: 3 }, collision_px: { x: 0, y: 0, width: 144, height: 144 }, visual_bounds_px: null } },
  { building_type: 'wall_factory', name: 'Wall Factory', cost: 50000, prerequisite: 'wall_research', status: 'specified', geometry: { footprint_tiles: { x: 3, y: 3 }, collision_px: { x: 0, y: 0, width: 144, height: 96 }, visual_bounds_px: null } },
] };
const ownership = { game_id: id, town_id: 'town', owner_player_id: id };
const town = { id: 'town', name: 'Copper Creek', mayor_player_id: id, hall: { id: 'hall-site', cell: [0, 0], origin: { x: 16, y: 16 } } };

async function setup(page: Page, options: { cash?: number; failCatalog?: boolean } = {}) {
  const state = {
    cash: options.cash ?? 55000, revision: 1, records: [] as OwnedBuilding[], inputs: [] as Record<string, unknown>[],
    commands: [] as ConstructionCommand[], outcomes: new Map<string, ConstructionResult>(), x: 816, y: 912, hp: 100,
    unknownNext: false, rejectNext: null as string | null, failCatalog: options.failCatalog ?? false, errors: [] as string[],
  };
  let active: WebSocketRoute | undefined;
  const send = (value: unknown) => active?.send(JSON.stringify(value));
  const frames = () => {
    const reason = state.hp <= 0 ? 'not_alive' : state.x < 0 || state.x >= 1536 || state.y < 0 || state.y >= 1536 ? 'outside_territory' : null;
    const territory = { min_tile: { x: 0, y: 0 }, size_tiles: { x: 32, y: 32 } };
    const hall = { id: 'hall-entity', building_type: 'town_hall', origin_tile: { x: 16, y: 16 }, ownership,
      health: { current: 200, maximum: 200 }, production: null, geometry: catalog.buildings[0].geometry, town_name: town.name };
    send({ type: 'state', t: Date.now(), me: id, players: [[id, state.x, state.y, 0, state.hp]], bullets: [], towns: [town], catalog_version: 2,
      tiles: { ox: Math.floor(state.x / 48) - 13, oy: Math.floor(state.y / 48) - 9, w: 26, h: 18, data: Array(468).fill(0) },
      self_state: { identity: { game_id: id, town_id: 'town', role: 'mayor' }, wallet: { cash: state.cash }, inventory: { slots: Array(6).fill(null) }, cooldowns: { primary_fire_ready_at_ms: 0, item_use_ready_at_ms: 0 } },
      buildings: [hall, ...state.records.map(r => ({ ...r.building, geometry: catalog.buildings.find(b => b.building_type === r.building.building_type)!.geometry, town_name: null }))],
      construction: { territory, can_construct: reason === null, reason },
    });
    send({ type: 'economy_state', cash: state.cash, revision: state.revision, territory, owned_buildings: state.records,
      constructible_types: ['wall_research', 'wall_factory'], demolition_refund_percent: 25 });
  };
  page.on('pageerror', error => state.errors.push(error.message));
  await page.route('https://fonts.googleapis.com/**', route => route.abort());
  await page.route('https://fonts.gstatic.com/**', route => route.abort());
  await page.route('**/@auth0_auth0-react.js*', route => route.fulfill({ contentType: 'application/javascript', body: `
    export const Auth0Provider=({children})=>children;
    export const useAuth0=()=>({isLoading:false,isAuthenticated:true,user:{name:'Test Mayor'},getAccessTokenSilently:async()=>'',getIdTokenClaims:async()=>({__raw:'fixture-token'}),logout:async()=>{}});
  ` }));
  await page.route('**/catalog', route => {
    if (state.failCatalog) return route.fulfill({ status: 503, headers: { 'access-control-allow-origin': '*' }, body: 'temporarily unavailable' });
    return route.fulfill({ contentType: 'application/json', headers: { 'access-control-allow-origin': '*' }, body: JSON.stringify(catalog) });
  });
  await page.routeWebSocket(/\/play$/, ws => {
    ws.onMessage(data => {
      const message = JSON.parse(data.toString());
      if (message.type === 'check_game') ws.send(JSON.stringify({ type: 'existing_game', game: { player_id: id, town_id: 'town', town_name: town.name, is_connected: false, reconnect_until_ms: null } }));
      else if (message.type === 'resume_game') { active = ws; frames(); }
      else if (message.type === 'input') state.inputs.push(message);
      else if (message.type === 'ping') ws.send(JSON.stringify({ type: 'pong' }));
      else if (message.type === 'quit_game') ws.send(JSON.stringify({ type: 'game_quit_ack' }));
      else if (message.type === 'place_building' || message.type === 'demolish_building') {
        state.commands.push(message);
        let result = state.outcomes.get(message.request_id);
        if (!result) {
          result = { request_id: message.request_id, error: state.rejectNext, building_id: null, cash: state.cash, refund: 0 };
          state.rejectNext = null;
          if (!result.error && message.type === 'place_building') {
            const definition = catalog.buildings.find(b => b.building_type === message.building_type)!;
            const buildingId = `building-${state.records.length + 1}`;
            state.cash -= definition.cost;
            state.records.push({ price_paid: definition.cost, building: { id: buildingId, building_type: definition.building_type,
              ownership, origin_tile: message.origin_tile, health: { current: 255, maximum: 255 }, production: null } });
            result.building_id = buildingId;
          } else if (!result.error) {
            const record = state.records.find(r => r.building.id === message.building_id)!;
            result.refund = Math.floor(record.price_paid / 4); state.cash += result.refund;
            state.records = state.records.filter(r => r.building.id !== message.building_id); result.building_id = message.building_id;
          }
          result.cash = state.cash; state.revision++; state.outcomes.set(message.request_id, result);
        }
        frames();
        if (state.unknownNext) {
          state.unknownNext = false;
          send({ type: 'construction_result', outcome_known: false, result: { ...result, error: 'service_unavailable', building_id: null } });
        } else send({ type: 'construction_result', outcome_known: true, result });
      }
    });
  });
  const resume = async () => {
    await page.getByRole('button', { name: /Resume town/ }).click();
    await expect(page.getByRole('heading', { name: 'Build your foothold.' })).toBeVisible();
    await expect(page.getByTestId('cash-value')).toHaveText(state.cash.toLocaleString('en-US'));
  };
  await page.goto('/', { waitUntil: 'domcontentloaded' });
  await resume();
  return { state, frames, resume };
}

async function anchor(page: Page, x: number, y: number, playerX = 816, playerY = 912) {
  const canvas = page.locator('canvas');
  await canvas.scrollIntoViewIfNeeded();
  const box = (await canvas.boundingBox())!;
  const scale = Math.min(box.width / (24 * 48), box.height / (16 * 48));
  const px = box.x + box.width / 2 + (x * 48 + 2 - playerX) * scale;
  const py = box.y + box.height / 2 + (y * 48 + 2 - playerY) * scale;
  await page.mouse.move(px, py); await page.mouse.click(px, py);
}

test('catalog failure retries, income updates HUD, and research locks explain eligibility', async ({ page }) => {
  const { state, frames } = await setup(page, { cash: 1000, failCatalog: true });
  await expect(page.getByRole('alert').filter({ hasText: 'Catalog request failed' })).toBeVisible();
  state.failCatalog = false;
  await page.getByRole('button', { name: 'Retry catalog' }).click();
  await expect(page.getByRole('button', { name: 'Place Wall Research' })).toBeDisabled();
  await expect(page.getByText('Need 4,000 more cash.')).toBeVisible();
  await expect(page.getByText('Requires Wall Research.')).toBeVisible();
  state.cash = 10000; state.revision++; frames();
  await expect(page.getByTestId('cash-value')).toHaveText('10,000');
  await expect(page.getByRole('button', { name: 'Place Wall Research' })).toBeEnabled();
  state.hp = 0; frames();
  await expect(page.getByRole('button', { name: 'Place Wall Research' })).toBeDisabled();
  await expect(page.getByText('Wait until you respawn to build or demolish.').first()).toBeVisible();
  state.hp = 100; state.x = -1; frames();
  await expect(page.getByText('Return to your town territory to build or demolish.').first()).toBeVisible();
  expect(state.errors).toEqual([]);
});

test('placement previews, cancellation and tool clicks never fire or purchase implicitly', async ({ page }) => {
  const { state, frames } = await setup(page);
  await page.getByRole('button', { name: 'Place Wall Research' }).click();
  await anchor(page, 16, 18);
  await expect(page.getByRole('status').filter({ hasText: 'Reserved building footprints cannot overlap' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Confirm placement' })).toBeDisabled();
  expect(state.commands).toHaveLength(0);
  await page.getByRole('button', { name: 'Move anchor' }).click();
  state.x = 29 * 48; frames();
  await anchor(page, 31, 16, state.x);
  await expect(page.getByRole('status').filter({ hasText: 'entire footprint must stay inside' })).toBeVisible();
  await page.getByRole('button', { name: 'Move anchor' }).click();
  state.x = 816; frames();
  await anchor(page, 13, 16);
  await expect(page.getByRole('button', { name: 'Confirm placement' })).toBeEnabled();
  await page.screenshot({ path: test.info().outputPath('construction-desktop.png') });
  await page.keyboard.down('Space'); await page.waitForTimeout(160);
  expect(state.inputs.slice(-2).every(input => input.shoot === false)).toBeTruthy();
  await page.keyboard.press('Escape'); await page.waitForTimeout(160);
  expect(state.inputs.slice(-2).every(input => input.shoot === false)).toBeTruthy();
  await page.keyboard.up('Space');
  await expect(page.getByRole('button', { name: 'Confirm placement' })).toHaveCount(0);
  expect(state.commands).toHaveLength(0);
  await page.keyboard.down('KeyW'); await page.waitForTimeout(120);
  expect(state.inputs.at(-1)?.up).toBe(true);
  await page.keyboard.up('KeyW');
  await page.mouse.down(); await page.waitForTimeout(100); expect(state.inputs.at(-1)?.shoot).toBe(true); await page.mouse.up();
  expect(state.errors).toEqual([]);
});

test('research/factory purchase and provenance-based demolition keep the existing factory', async ({ page }) => {
  const { state, frames } = await setup(page);
  await page.getByRole('button', { name: 'Place Wall Research' }).click(); await anchor(page, 13, 16);
  await page.getByRole('button', { name: 'Confirm placement' }).click();
  await expect(page.getByText('Building placed.', { exact: true })).toBeVisible();
  await expect(page.getByTestId('cash-value')).toHaveText('50,000');
  await expect(page.getByRole('button', { name: 'Place Wall Factory' })).toBeEnabled();
  await page.getByRole('button', { name: 'Place Wall Factory' }).click(); await anchor(page, 16, 13);
  await page.getByRole('button', { name: 'Confirm placement' }).click();
  await expect(page.getByTestId('cash-value')).toHaveText('0');
  expect(state.records).toHaveLength(2);
  // Simulate a historical price differing from the current catalog.
  state.records[0].price_paid = 8003; state.revision++; frames();
  await page.getByRole('button', { name: 'Inspect & demolish' }).click();
  await page.getByRole('list', { name: 'Your infrastructure' }).getByRole('button', { name: /Wall Research/ }).click();
  await expect(page.getByText('Paid 8,003 · Refund', { exact: false })).toContainText('2,000');
  await page.getByRole('button', { name: 'Demolish selected building' }).click();
  await page.getByRole('button', { name: 'Keep building' }).click(); expect(state.records).toHaveLength(2);
  await page.getByRole('button', { name: 'Demolish selected building' }).click();
  await page.getByRole('button', { name: 'Confirm demolition' }).click();
  await expect(page.getByText('Building demolished. 2,000 cash refunded.', { exact: true })).toBeVisible();
  await expect(page.getByTestId('cash-value')).toHaveText('2,000');
  expect(state.records.map(r => r.building.building_type)).toEqual(['wall_factory']);
  await expect(page.getByText('Requires Wall Research.')).toBeVisible();
  expect(state.errors).toEqual([]);
});

test('uncertain result survives reload and safely replays the exact request without rolling back the HUD', async ({ page }) => {
  const { state, frames, resume } = await setup(page);
  state.unknownNext = true;
  await page.getByRole('button', { name: 'Place Wall Research' }).click(); await anchor(page, 13, 16);
  await page.getByRole('button', { name: 'Confirm placement' }).click();
  await expect(page.getByRole('button', { name: 'Retry saved request' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Place Wall Factory' })).toBeDisabled();
  const original = state.commands[0];
  state.cash += 10000; state.revision++; frames();
  await expect(page.getByTestId('cash-value')).toHaveText('60,000');
  await page.reload({ waitUntil: 'domcontentloaded' }); await resume();
  await expect(page.getByRole('button', { name: 'Retry saved request' })).toBeVisible();
  await page.getByRole('button', { name: 'Retry saved request' }).click();
  await expect(page.getByText('Building placed.', { exact: true })).toBeVisible();
  expect(state.commands).toHaveLength(2); expect(state.commands[1]).toEqual(original);
  expect(state.records).toHaveLength(1);
  await expect(page.getByTestId('cash-value')).toHaveText('60,000');
  expect(state.errors).toEqual([]);
});

test('server rejections remain visible, townhalls stay protected, and mobile layout does not overflow', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const { state, frames } = await setup(page);
  state.rejectNext = 'occupied';
  await page.getByRole('button', { name: 'Place Wall Research' }).click(); await anchor(page, 13, 16);
  await page.getByRole('button', { name: 'Confirm placement' }).click();
  await expect(page.getByRole('alert').filter({ hasText: 'That footprint is already occupied.' })).toBeVisible();
  await page.getByRole('button', { name: 'Cancel placement' }).click();
  await page.getByRole('button', { name: 'Inspect & demolish' }).click();
  await anchor(page, 16, 16);
  await expect(page.getByRole('heading', { name: 'Town Hall', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Demolish selected building' })).toBeDisabled();
  state.cash = Number.MAX_SAFE_INTEGER; state.revision++; frames();
  await expect(page.getByTestId('cash-value')).toHaveText('9,007,199,254,740,991');
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  await page.screenshot({ path: test.info().outputPath('construction-mobile.png'), fullPage: true });
  expect(state.commands).toHaveLength(1);
  expect(state.errors).toEqual([]);
});
