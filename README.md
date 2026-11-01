# This is the server softwares for the Townwarz game.

See [PHASE2.md](PHASE2.md) for physical neutral/owned townhalls, exact 3x3/96px-body/48px-forecourt geometry, building occupancy/collision, and the **15-minute same-game reconnect lifecycle**. Intentional quitting uses a no-successor release until promotion candidates are implemented.

See [PHASE1.md](PHASE1.md) for the versioned Buildings/Items/ItemBuildings domain model, player-owned wallets and six-slot inventory, town allegiance, reward/assist extension points, and handoff-state guarantees. The read-only catalog is available at gateway `GET /catalog`.

## New-game flow and local configuration

The React client now uses **`/play`** on the gateway. After Auth0 login it asks for a role (Mayor only for now) and a town name. The first WebSocket frame is:

```json
{"v":1,"type":"start_game","id_token":"<Auth0 ID token>","role":"mayor","town_name":"Copper Creek"}
```

The gateway validates RS256 signature, issuer, SPA client-ID audience, and token expiry using Auth0 JWKS. It creates a fresh player UUID and asks the conductor to allocate a townhall, then connects directly to that hall's owning cell server. Cell servers receive the town in the initial trusted `set_pid` frame, spawn beside the correct generated hall, preserve the home spawn through handoffs, and include active `towns` in game snapshots. Townhall locations use the same 32-tile grid as terrain generation, with 48-pixel tiles and 64-tile routing cells.

Copy `.env.example` to **`.env` in the workspace root** and configure:

```dotenv
DATABASE_URL=postgres://postgres:your-password@127.0.0.1:5432/townwarz
AUTH0_DOMAIN=your-tenant.us.auth0.com
AUTH0_CLIENT_ID=your-client-id
```

- Create the `townwarz` PostgreSQL database first. The local `affectionate_shtern` container's `townwarz` database was created during implementation; other databases were left alone.
- The conductor requires `DATABASE_URL` and applies `twz-conductor/migrations/001_games.sql` automatically. This initial local implementation uses an unencrypted PostgreSQL connection (`NoTls`).
- The gateway requires the two Auth0 values for `/play`. They must match the web client's `VITE_AUTH0_DOMAIN` and `VITE_AUTH0_CLIENT_ID`. Use the Auth0 hostname without `https://`. No client secret or additional Auth0 API audience is required. The SPA must use RS256 signing.
- Run `cargo run` commands from the workspace root so dotenv finds `.env`. Environment variables take precedence over dotenv.
- `twz_users` stores verified Auth0 identity/profile data; `twz_games` stores game role, town details, and start/end times. Town names are trimmed and validated server-side (2–32 Unicode characters; letters, numbers, spaces, apostrophes, hyphens). Names need not be unique.
- Unexpected disconnects preserve the game/town for **15 minutes**. Authenticated `/play` `resume_game` entry restores its checkpoint, unless it has expired, been intentionally left, or ended (e.g. Orbed). `start_game` creates a new town only when no game remains reserved for that account. Normal combat death still respawns at its own hall.
- Intentional Leave game or Sign out explicitly ends the current mayor game and releases the hall through today's no-successor outcome. Future eligible members must instead receive promotion offers. Game history is retained. Gateway leases become disconnected after 45 seconds without heartbeats, then follow the 15-minute grace policy rather than immediately ending the game. Checkpoints are stored every 10 seconds and at ordinary disconnect.
- Cell servers refresh active town labels every two seconds; the newly created own-town label is also installed immediately at spawn. Ended towns disappear from snapshots on the next refresh.
- `/ws` is retained for the anonymous `debug-client.html`; it uses the shared fallback spawn and does not create a persisted town. It is a development endpoint, not an authenticated game entry.

### Docker configuration

For a conductor container, supply `--env-file .env` (or explicit `-e DATABASE_URL=...`) and use a database hostname reachable **from the container**, e.g. `host.docker.internal:5432` on Docker Desktop. The Docker supervisor forwards `AUTH0_DOMAIN` and `AUTH0_CLIENT_ID` from its own environment into the gateway. Supply all three values to the conductor container, rebuild the service images after Rust changes, and recreate existing containers so new environment settings take effect.

### Verification

```sh
cargo check --workspace
cargo test --workspace
```

The PostgreSQL integration test is opt-in and uses a unique fixture user, cleaning up its own records afterwards. Point it at a development/test database and NATS instance:

```powershell
$env:TEST_DATABASE_URL = $env:DATABASE_URL
$env:TEST_NATS_URL = 'nats://127.0.0.1:4222'
cargo test --workspace postgres_grace -- --ignored --nocapture
```

The Auth0 JWT validation tests run in the normal workspace suite without network access. Phase 2's PostgreSQL grace/resume test uses `cargo test --workspace postgres_grace -- --ignored --nocapture` with TEST_DATABASE_URL/TEST_NATS_URL.

For the live-cell integration test, also set `TEST_CONDUCTOR_URL` to an isolated running conductor with registered cells backed by the test database. Run `cargo test --workspace authenticated_entry_live_cells_geometry -- --ignored --nocapture`. It verifies actual hall geometry, resumed movement/state, Orb invalidation, stable hall reuse and acknowledged quit. To run just the stub-service test on a separate isolated NATS instance, use the filter `authenticated_entry_verifies`.

## Here's an overview of how the architecture is supposed to work:
- Many clients talk to the game gateway.
- The game gateway talks to many cell servers.
- Each cell server acts as an authoritative source for game state, and manages inputs and state updates for the clients, then retransmits the states to each client within its "cell".
- There is a conductor behind the scenes that decides which clients use which cell server. Eventually the conductor will also handle scaling the app by provisioning new resources and spawning new cell server instances.
- All of the interprocess communication happens via a NATS server.

## To run the app, you need to start all the various processes in order. That order is:
- Conductor
- NATS server
- Cell server A
- Cell server B
- Gateway

## The current commands used for local dev are:
```Conductor
cargo run -p twz-conductor -- --bind 0.0.0.0:7000
```
```NATS server
nats-server -p 4222 -js
```
```Cell server A
cargo run -p twz-cellserver -- --id A --bind 127.0.0.1:3001 --conductor http://127.0.0.1:7000 --nats nats://127.0.0.1:4222
```
```Cell server B
cargo run -p twz-cellserver -- --id B --bind 127.0.0.1:3002 --conductor http://127.0.0.1:7000 --nats nats://127.0.0.1:4222
```
```Gateway
cargo run -p twz-gateway -- --bind 0.0.0.0:8081 --conductor http://127.0.0.1:7000 --nats nats://127.0.0.1:4222
```

Alternatively, you can use docker to spin up the conductor, which will itself spin up the necessary containers. For now, the conductor toml is configured to run 2 cell servers. Each container image will need to be built with separate commands:

```Conductor
docker build -t twz-conductor:local --build-arg BIN=twz-conductor -f twz-conductor/Dockerfile .
```

```Gateway
docker build -t twz-gateway:local --build-arg BIN=twz-gateway -f twz-gateway/Dockerfile .
```

```Cellserver
docker build -t twz-cellserver:local --build-arg BIN=twz-cellserver -f twz-cellserver/Dockerfile .
```

Then the actual conductor can be run with:
```Powershell
docker run --rm -it --name twz-conductor --user 0:0 --network twz_net -v /var/run/docker.sock:/var/run/docker.sock -v "$PWD\twz-conductor\conductor.toml:/app/conductor.toml" -p 7000:7000 -e RUST_BACKTRACE=1 -e RUST_LOG=info twz-conductor:local --bind 0.0.0.0:7000 --nats nats://twz-nats:4222 --supervise --config /app/conductor.toml
```
