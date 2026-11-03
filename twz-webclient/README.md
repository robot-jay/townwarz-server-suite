# TownWarz Web Client

React + TypeScript + Vite client in the TownWarz monorepo's `twz-webclient/` directory. After Auth0 sign-in, inspect your existing town and choose whether to resume it or abandon it and start over. When no town is reserved, choose Mayor and name a new town. Physical townhalls have a 3x3 footprint and a one-row walkable forecourt. Owned town names appear above their halls.

## Quick start

Requires Node.js 20.19+ or 22.12+ (a current LTS is recommended).

Run the commands below from `twz-webclient/`. From the repository root, use `npm --prefix twz-webclient` in place of `npm`.

```sh
npm ci
```

Copy this directory's `.env.example` to `twz-webclient/.env` (separate from the Rust services' root `.env`), fill in the values below, then run:

```sh
npm run dev
```

Open **http://localhost:5173**. Missing Auth0 configuration displays a setup screen rather than opening a game connection.

## Auth0 setup

Create an Auth0 application of type **Single Page Application**. In its settings, configure:

| Setting | Local value |
| --- | --- |
| Allowed Callback URLs | `http://localhost:5173` |
| Allowed Logout URLs | `http://localhost:5173` |
| Allowed Web Origins | `http://localhost:5173` |

Set these values in `.env`:

```dotenv
VITE_AUTH0_DOMAIN=your-tenant.us.auth0.com
VITE_AUTH0_CLIENT_ID=your-client-id
VITE_AUTH0_REDIRECT_URI=http://localhost:5173
VITE_GAME_WS_URL=ws://localhost:8081/play
```

Use the tenant hostname for the domain (no `https://` prefix). The redirect URI is optional and defaults to the current origin. Restart Vite after editing environment variables. For deployment, add the deployed origin to all three Auth0 settings and update the redirect URI. An HTTPS page requires a `wss://` gateway endpoint.

`VITE_*` values are public build-time configuration. Do **not** put an Auth0 client secret here. `.env` and `.env.*` are gitignored, with `.env.example` deliberately tracked. Auth0 uses its default in-memory token cache and redirect-based Universal Login.

### Authentication scope

The `/play` gateway entry validates the Auth0 **ID token** signature (RS256), issuer, audience (the SPA client ID), and expiry before creating a game. No Auth0 API audience or client secret is needed for this first-party game entry. The token is sent in the initial WebSocket frame, not the URL. Use HTTPS/WSS outside local development.

Configure matching `AUTH0_DOMAIN` and `AUTH0_CLIENT_ID` in the **repository root's** `.env`, together with `DATABASE_URL`. See the [root README](../README.md). Users are keyed by the verified Auth0 subject; profile details and game/town history are stored in PostgreSQL.

Unexpected connection loss reserves your game and town for **15 minutes**. Login/refresh performs a read-only `check_game`, displaying the town name and connection status without claiming its session or opening a simulation connection. Choose **Resume town** for a disconnected game, or **Abandon town & start over** even if the town is active elsewhere. Abandonment uses an authenticated `abandon_game` request targeting that exact game and waits for server acknowledgement before showing new-game setup. If another tab starts a game during setup, a rejected creation displays the same choice instead of only an error.

Explicit Resume/Reconnect restores player identity, position, wallet, inventory, cooldowns, health and respawn state. A game that has expired or ended (including an Orb defeat) cannot be resumed. Normal combat death still respawns at that game's townhall and clears inventory, not cash. A stale abandonment request cannot release a newer game or another account's town.

When the latest game ended because the grace period elapsed, the entry screen names the former town and explains that you were away longer than the reconnect window. **Start a new game** acknowledges the notice server-side before opening role/name setup, so it stays dismissed across logins. An inactive former town is not offered; new halls are randomized normally. Future active-town metadata includes one Mayor and four Commando slots, but existing-town joining remains unavailable until membership is implemented. Rejoining after expiry will be fresh participation, not restoration of the expired wallet/inventory. See the server's `EXPIRY_ENTRY.md`.

**Leave game** and **Sign out** send an explicit quit and await acknowledgement. With no successor roles implemented, this releases the hall immediately; future promotion handling must offer the town to eligible members first. Refreshing or closing the page is treated as an unexpected disconnect, not an intentional quit. A crashed gateway is detected after roughly 45 seconds; reconnect may report that the old connection is still closing during that interval.

The original `/ws` endpoint remains an anonymous debug entry and does not create a persistent town. The React client upgrades an older `VITE_GAME_WS_URL` ending in `/ws` to `/play` automatically.

## Game server

Run PostgreSQL and the Rust services from this repository's root: NATS, conductor, registered cell servers, and gateway. The conductor applies its idempotent schema migration on startup. See the [root setup instructions](../README.md) for environment variables and Rust commands. Towns now use PostgreSQL instead of the old JetStream KV prototype.

The local gateway defaults to `ws://localhost:8081/play`. Its Docker supervisor config uses **8080**, so set `VITE_GAME_WS_URL=ws://localhost:8080/play` for that setup. The client talks only to the gateway; it does not need direct access to PostgreSQL, NATS, conductor, or individual cell servers.

## Controls & behavior

- **W/S** or **Up/Down**: move forward/backward.
- **A/D** or **Left/Right**: rotate.
- **Space** or **left mouse button**: fire. Shooting is edge-triggered on the server; release and press again for another shot.
- Click or tab into the world to focus controls. Inputs clear when focus is lost or the tab is hidden.
- The camera follows the server-authoritative player position. Terrain, players, bullets, health, connection status, ping, and sector coordinates are rendered from server snapshots.
- Death UI derives from authoritative snapshot HP. The server also emits combat notifications and defeat attribution for future effects/rewards. Respawn remains server-controlled.
- Input is sent every 50 ms; state arrives at the server's current network cadence. This initial client renders snapshots directly without prediction/interpolation.
- Dropped connections show a Reconnect option. Initial login/refresh checks for an existing town and offers resume or simulation-free abandonment. Explicit quit/logout releases the hall; creation remains initiated by form submission, avoiding duplicate allocation under Strict Mode.
- The view scales to fit within the server's fixed 26×18 tile window. Gameplay currently expects a keyboard/mouse.

## Commands

```sh
npm run dev        # Local development on port 5173
npm run typecheck  # Strict TypeScript checks
npm test           # Domain/snapshot boundary regression tests
npm run build      # Type-check and create dist/
npm run preview    # Serve the production build locally
```

For a static production host, publish `dist/` and configure fallback to `index.html` for Auth0 redirects. Set environment values before building; Vite embeds them in the generated assets.

Client tests and the production build run in the repository-root [GitHub Actions workflow](../.github/workflows/ci.yml) alongside the Rust checks on PR updates and pushes to `master`. There is no separate client repository or nested workflow to configure.

## Structure

```text
src/
  main.tsx                  Auth0 provider and bootstrap
  config.ts                 Public environment configuration
  App.tsx                   Configuration and Auth0 login screens
  Layout.tsx                Shared branding and page layout
  NewGame.tsx               Role selection, town naming, game shell
  styles.css                Responsive UI and landing-page artwork
  game/
    protocol.ts             Wire types and snapshot validation
    domain.ts               Versioned private player state and item identifiers
    protocol.test.ts        Domain/snapshot regression checks
    useGameConnection.ts    Authenticated game entry, socket lifecycle, input, ping
    GameCanvas.tsx          Canvas renderer and focused gameplay input
```

## Phase 1 domain state

Game snapshots now contain `catalog_version`, `self_state` (your identity, personal wallet, six-slot inventory and cooldowns), and public `player_affiliations`. The validator accepts older snapshots and checks these fields when present. Other players' wallets/inventories remain private.

Allegiance is town-based; anonymous debug players are independent enemies. Combat death clears inventory but retains cash. New games start with zero cash and empty inventory. The gateway's read-only `GET /catalog` provides server-owned definitions and prices for future construction UI. Bazooka rockets and homing missiles have distinct item IDs.

## Phase 2 buildings and reconnects

Catalog version 2 snapshots include `buildings`, with instance IDs, ownership, health, footprint, collision and visual bounds, and town names. The renderer uses these instead of terrain-tile hall markers. A hall reserves 144x144 pixels; its 144x96 body is solid and its bottom 144x48 row is walkable. Neutral halls remain physical after release. Friendly projectiles pass through owned buildings; hostile/neutral bodies block them.

The server applies `002_reconnect.sql` automatically. Rebuild/restart conductor, gateway and all cells together; no additional frontend .env settings are needed. See the server's `PHASE2.md` for checkpoint behavior, lifecycle fencing and promotion integration points.

These are domain/state foundations; construction, production, pickups and inventory controls are later phases. See the server suite's `PHASE1.md` for authority boundaries and the future teammate/assist reward model.
