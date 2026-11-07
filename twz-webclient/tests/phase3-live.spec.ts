import { expect, test } from '@playwright/test';
import type { Snapshot } from '../src/game/protocol';

// The Rust acceptance fixture supplies a gateway that verifies real signed
// fixture JWTs, a real conductor, two cell processes and isolated PostgreSQL/NATS.
// Only the external Auth0 login UI is replaced. Catalog/game traffic is real.
test('earn → research → factory → research demolition through the real stack', async ({ page }) => {
  test.setTimeout(60000);
  const token = process.env.TEST_REAL_ID_TOKEN;
  expect(token).toBeTruthy();
  let snapshot: Snapshot | null = null;
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('websocket', socket => socket.on('framereceived', frame => {
    try {
      const value = JSON.parse(frame.payload.toString());
      if (value.type === 'state') snapshot = value;
    } catch { /* Ignore non-JSON frames; the application validates its own input. */ }
  }));
  await page.route('https://fonts.googleapis.com/**', route => route.abort());
  await page.route('https://fonts.gstatic.com/**', route => route.abort());
  await page.route('**/@auth0_auth0-react.js*', route => route.fulfill({ contentType: 'application/javascript', body: `
    export const Auth0Provider=({children})=>children;
    export const useAuth0=()=>({isLoading:false,isAuthenticated:true,user:{name:'Acceptance Mayor'},getAccessTokenSilently:async()=>'',getIdTokenClaims:async()=>({__raw:${JSON.stringify(token)}}),logout:async()=>{}});
  ` }));
  await page.goto('/', { waitUntil: 'domcontentloaded' });
  await page.getByRole('button', { name: /Become a mayor/ }).click();
  await page.getByLabel('Town name').fill('Phase Three Accepted');
  await page.getByRole('button', { name: /Found town & enter world/ }).click();
  await expect(page.getByRole('heading', { name: 'Build your foothold.' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Place Wall Research' })).toBeEnabled();
  const choose = async (type: 'Wall Research' | 'Wall Factory', dx: number, dy: number) => {
    await page.getByRole('button', { name: `Place ${type}` }).click();
    const state = snapshot!;
    const me = state.players.find(p => p[0] === state.me)!;
    const town = state.towns!.find(t => t.mayor_player_id === state.me)!;
    const canvas = page.locator('canvas');
    const box = (await canvas.boundingBox())!;
    const scale = Math.min(box.width / (24 * 48), box.height / (16 * 48));
    await page.mouse.click(box.x + box.width / 2 + ((town.hall.origin.x + dx) * 48 + 2 - me[1]) * scale,
      box.y + box.height / 2 + ((town.hall.origin.y + dy) * 48 + 2 - me[2]) * scale);
    await expect(page.getByRole('button', { name: 'Confirm placement' })).toBeEnabled();
  };
  await choose('Wall Research', -3, 0);
  await page.getByRole('button', { name: 'Cancel placement' }).click();
  await choose('Wall Research', -3, 0);
  await page.getByRole('button', { name: 'Confirm placement' }).click();
  await expect(page.getByText('Building placed.', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Place Wall Factory' })).toBeEnabled();
  await choose('Wall Factory', 0, -3);
  await page.getByRole('button', { name: 'Confirm placement' }).click();
  await expect(page.getByText('Building placed.', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Inspect & demolish' }).click();
  const infrastructure = page.getByRole('list', { name: 'Your infrastructure' });
  await expect(infrastructure.getByRole('button')).toHaveCount(2);
  await infrastructure.getByRole('button', { name: /Wall Research/ }).click();
  await page.getByRole('button', { name: 'Demolish selected building' }).click();
  await page.getByRole('button', { name: 'Confirm demolition' }).click();
  await expect(page.getByText('Building demolished. 1,250 cash refunded.', { exact: true })).toBeVisible();
  await expect(page.getByText('Requires Wall Research.')).toBeVisible();
  await page.getByRole('button', { name: 'Inspect & demolish' }).click();
  await expect(infrastructure.getByRole('button')).toHaveCount(1);
  await expect(infrastructure.getByRole('button', { name: /Wall Factory/ })).toBeVisible();
  expect(errors).toEqual([]);
  await page.screenshot({ path: test.info().outputPath('phase3-live-accepted.png') });
});
