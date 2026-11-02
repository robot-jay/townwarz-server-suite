# Phase 2: physical buildings and reconnect lifecycle

## Agreed behavior

- Townhalls occupy a **3x3 tile / 144x144 pixel** reserved footprint. The **144x96 pixel upper body** blocks movement and hostile projectiles. The bottom **144x48 pixel row is a walkable forecourt**, still reserved against construction overlap.
- Every procedural hall site has a visible, physical neutral hall, materialized lazily with terrain chunks. Claiming changes ownership/name, not its stable entity identity. Ending a town returns its hall to neutral.
- Fresh games choose a random free hall from an initial 4x4 grid of sites, expanding with live/reserved claims. The same account does not receive its most recent hall on its next fresh game. Resuming a game retains its original hall and checkpoint; active and grace-reserved halls cannot be reassigned.
- Players collide with all buildings. A projectile skips buildings with the same non-null town ID; enemy and neutral bodies block it. Neutral does not mean a shared anonymous faction.
- An **unexpected disconnect reserves the same game/town for 15 minutes**. Reconnecting with the same verified Auth0 subject restores that game, unless it has ended (including an Orb defeat).
- Intentional Leave game or Sign out starts the mayor-departure path. Eventually that path must offer promotions to eligible successors until none remain. Currently no successor roles are playable, so its explicit no-successor outcome releases the hall immediately. This phase does not implement promotion selection/timers.
- Timeout abandonment uses that same no-successor outcome. Orb defeat is terminal, not a promotion or reconnect opportunity.

## Buildings and geometry

`twz-shared/domain/geometry.rs` defines world rectangles, circle ejection (including an embedded center), swept segment collision, and deterministic coordinate-based hall UUIDs. Catalog version is now **2**. Normal building geometry has top-left tile anchors, separate reservation/collision/visual bounds, and a one-row forecourt on the short-body types. Players retain world-space center coordinates; their existing spawn offset remains unchanged and lies safely below the hall's solid body.

`twz-cellserver/src/buildings.rs` stores building instances and indexes every occupied tile, including the forecourt. Insertions validate the complete footprint before committing an instance or occupancy. Removals clear occupancy and associated factory pickups. Neutral hall ownership is `None`; owned instances carry explicit game/town/player IDs. Owner-based cleanup removes ordinary buildings, pickups, deployments, and production outputs without deleting the physical hall. A stale cleanup for an old game cannot remove a new claim of that same hall.

Terrain contains ground/water/rock, rather than embedding the hall collider in a terrain tile. Chunk loading creates neutral hall entities. Nearby `buildings` views contain instance identity, ownership, health, origin, geometry, and townhall name. React and the static debug client render these views, with an actual one-row forecourt. React retains a legacy tile-rendering fallback for older snapshots.

All building changes run through the cell simulation's control channel. Internal InsertBuilding/RemoveBuilding commands provide creation/removal foundations for Phase 3; they are **not public construction or free-building APIs**. Creation checks active ownership, content availability, terrain, and full occupancy, and derives initial health from the server catalog. Hall claims/releases are lifecycle-managed. The tests exercise ordinary factory creation/removal as well as hall generation/claiming.

Movement includes building collision, and physics delta is capped at 100ms to avoid large player integration steps after stalls. Bullets use a swept segment/body intersection rather than checking only their endpoint. Building damage, destruction rewards, manufacturing, purchases, demolition UI, and deployment behavior are later phases. Health/destructibility are modeled; Phase 2 projectiles stop on hostile bodies but do not yet damage buildings.

## Lifecycle, persistence and fencing

Migration `twz-conductor/migrations/002_reconnect.sql` adds connection token, disconnect time, player checkpoint JSON, and end reason. It is applied automatically on conductor startup. Existing user and game history is retained. The current playable model permits one active mayor game per Auth0 subject; another live connection is rejected rather than duplicating an avatar.

The gateway captures private authoritative player snapshots in memory, checkpoints to PostgreSQL on its 10-second heartbeat, and asks the current cell for a final paused checkpoint when a browser disconnects. That checkpoint preserves position, health/respawn timer, wallet, inventory, cooldowns, home spawn, identity, and damage contributions. The cell removes the avatar without clearing the retained town/assets. Resume routes to the checkpoint's current cell and restores it before attaching the socket.

Normal browser disconnects use a final simulation checkpoint. If a gateway/backend fails before one can be obtained, the last persisted heartbeat checkpoint is the recovery fallback (up to roughly 10 seconds old during normal operation). Newly created games already have an initial checkpoint. Connection tokens change on resume: delayed heartbeats/disconnects from an older connection cannot overwrite or disconnect the resumed game. Cell socket cleanup also verifies that it still owns that player's outbound channel before despawning, preventing stale-reader cleanup from removing a newly attached avatar.

A gateway lease with no heartbeat for 45 seconds becomes disconnected and releases its connection token. The last heartbeat starts its 15-minute grace period. A resume immediately after a crashed gateway can therefore report that the previous connection is still closing until this stale-lease detection runs. Expiry is checked every 10 seconds. Ended rows cannot be revived by resume, a late checkpoint, or a late heartbeat.

The end operation publishes `twz.game.ended` with game ID/reason; cells retire matching state through their control queue. Active-town directory refresh every two seconds provides reconciliation after startup/reconnect or a missed notification. Local claim timestamps prevent an older directory response from undoing a new claim; ended-game tombstones reject stale adoption and directory entries. Hall reuse removes old-game ownership/entities before installing the new claim, with deterministic site identity preventing duplicate halls.

Allocation creation timestamps are strictly increasing per hall site (max of current wall time and prior claim plus one millisecond). Cells retain the latest claim generation even after neutralization, rejecting delayed initial attachments for older or retired claims as well as stale cleanup messages.

Ordinary building instances are active simulation state. Disconnect grace retains them on their cell; durable dynamic-building recovery across a cell-process crash is still future world persistence work. The currently playable hall itself is reconstructed from its retained town record. Full cross-cell dynamic-entity replication, distributed fencing, and projectile migration remain Phase 6 work.

## Wire flow

Initial `/play` messages all carry an Auth0 ID token:

- `start_game`: Mayor and town name, creates a fresh game only if the account has no reserved game.
- `check_game`: read-only metadata lookup. Returns `existing_game` with town name, game ID and connection/grace status, or `no_game`; it does not claim a token or connect to a cell.
- `resume_game`: looks up the authenticated subject; returns `no_game` or attaches to its reserved game.
- `abandon_game`: explicit menu action targeting the displayed player/game ID. Uses the same intentional departure/promotion path, scoped to that authenticated subject and game. It does not open a simulation connection, and an old menu cannot abandon a newer game.
- `quit_game`: authenticated menu/logout path, ends that subject's game even when its gameplay socket was lost.

During play, `quit_game` requires no token payload because the connection is already authenticated. The gateway waits for the conductor to confirm the intentional end and sends `game_quit_ack`. The client waits for that acknowledgement before leaving. Logout attempts the same explicit departure before Auth0 logout; if the server is unreachable, no network protocol can deliver intent, so the disconnect grace/expiry is the fallback.

New shared subjects: `twz.game.resume`, `twz.game.disconnect`, `twz.game.quit_user`, `twz.game.ended`, and `twz.town.promotion.requested`. The promotion hook includes former mayor, reason, empty eligible successors, and `no_successors` outcome. Future promotion implementation must replace that outcome when actual eligible members exist, rather than unconditionally releasing a town.

Backend `set_pid` now optionally carries `resume_state`. It is sent only on initial/resumed entry, not again during a server handoff (which must preserve the destination's newer adopted state). Private state snapshots include `resume_state`; the gateway, not the browser, chooses what gets stored. Internal `pause_game`/`game_checkpoint` frames request a final snapshot. Browser-supplied pause commands are not forwarded.

## Client behavior

After Auth0 login or page refresh, the React client checks for an existing game without entering it. Its entry screen offers explicit Resume or Abandon actions, showing town name and whether it is connected elsewhere. A create conflict also returns that choice. Abandonment waits for acknowledgement, retaining the town and showing retryable errors if it fails. An unexpected drop shows Reconnect and explicit abandonment options. A successful reconnect preserves the same player/game/town. A terminal `game_ended` notification disables reconnect and offers new-game setup. Unclaimed halls stay physical; a distinct unoccupied art treatment is deliberately deferred.

## Deployment and checks

Rebuild/restart **conductor, gateway, and all cell servers together**. The old 45-second terminal lease policy and fresh-game-only client are superseded. No new .env keys are required. The migration runs automatically with the existing DATABASE_URL.

```sh
cargo check --workspace
cargo test --workspace
```

With an isolated development PostgreSQL database:

```powershell
cargo test --workspace postgres_grace -- --ignored --nocapture
```

Set TEST_DATABASE_URL first. This repository test no longer requires NATS. It verifies checkpoint persistence, connection fencing, hold/reuse, intentional quit, Orb termination, and the 15-minute expiry using an aged fixture timestamp rather than a real wait. Use the `postgres_` filter to also run the independent-pool concurrency checks; see [PERSISTENCE.md](PERSISTENCE.md).

With an isolated running conductor/cells and TEST_CONDUCTOR_URL/TEST_NATS_URL:

```powershell
cargo test --workspace authenticated_entry_live_cells_geometry -- --ignored --nocapture
```

This verifies real physical hall snapshots, movement followed by same-game reconnection, terminal Orb invalidation, a different hall for a fresh game, and acknowledged intentional quit/abandonment. Stable site identity and safe hall reuse are covered by building-store tests. Frontend `npm test` and `npm run build` cover geometry/state validation and bundling; browser smoke checks cover drawing dimensions, login probing, reconnect/quit/logout and terminal-game UI.
