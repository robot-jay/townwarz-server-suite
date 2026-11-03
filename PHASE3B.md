# Phase 3b — authoritative income and durable construction

Phase 3a's rules are recorded in [ROADMAP.md](ROADMAP.md). This phase implements the server economy and command path. The React construction controls are Phase 3c; UI-level acceptance is Phase 3d. Manufacturing, pickups, inventory controls and Wall deployment remain Phase 4.

## Implemented rules

- New participations start at zero cash. Connected Mayors earn **100 per 3,000 eligible milliseconds**, including death/respawn. The first award requires a completed interval. Disconnected time earns nothing; the fractional interval survives reconnect. Handoffs do not restart the income clock.
- Only **Wall Research (5,000)** and **Wall Factory (50,000)** are purchasable. Existing Wall Research is required to buy a factory. Removing research does not remove or invalidate existing factories.
- Each hall owns a tile-aligned **32×32** territory, using Euclidean division for negative coordinates. The complete reserved footprint must fit; boundaries use exclusive maximum coordinates. This includes the walkable factory forecourt.
- A living Mayor must be inside their own territory to build/demolish there. Server definitions supply ownership, geometry, health and prices. Terrain, occupancy and living-player overlap are checked by the simulation. A player solely on the walkable forecourt does not obstruct placement.
- Townhalls cannot be demolished. Voluntary demolition refunds **floor(original price paid / 4)**: 1,250 for Wall Research and 12,500 for Wall Factory at normal prices. Lifecycle cleanup grants no refund.

## Authority and persistence

Migration `004_economy.sql` adds nullable `twz_games.economy_json`. It is automatically applied by `GameRepository` with the existing migration lock. The durable record contains cash, income progress, connection-scoped cumulative eligible time, revision, pinned home-cell authority, purchased buildings with original prices, and request outcomes. An ended game's record remains historical; it is excluded from recovery projections.

Final regression verification also identified the cause of the previously intermittent expiry-history failure: a VM/database wall-clock correction reordered `started_at` timestamps. Migration `005_participation_order.sql` adds a sequence-backed participation order; latest participation and previous-hall selection use it instead of wall-clock time. Existing history is backfilled using its recorded timestamps, and subsequent ordering is monotonic. Migration startup also takes the allocation lock so sequence initialization cannot race new game creation.

The game row is the economic serialization boundary. SeaORM reads it `FOR UPDATE` and updates it transactionally. Economic mutations and lifecycle operations therefore cannot spend the same cash or revive an ended row concurrently. The record is initialized lazily from an existing game's checkpoint for upgrade compatibility. Once initialized, its cash overrides movement-checkpoint cash on resume. Subsequent movement snapshots cannot overwrite the durable economic state.

The gateway supplies cumulative connected-session time with the verified player ID and connection token. Browsers cannot submit income amounts or elapsed time. The conductor converts only the newly observed eligible milliseconds into cash; repeated/lower elapsed values do not pay twice, and a new connection token starts a new cumulative clock while preserving the durable fractional interval. The gateway sends a final time sample on ordinary disconnect, before pause/checkpoint IO. An abrupt gateway failure retains the last committed time sample, not a guessed offline payment.

Income is independent of the avatar's current cell, so a player handoff does not transfer a second economic authority. Future rewards and production-related spending must use this boundary rather than directly changing a transient wallet.

## Construction transaction and recovery

1. The gateway parses a construction intention and supplies the authenticated participation and connection token.
2. The conductor locks the game row, fences ended/stale connections, accounts for eligible income, and checks the request ledger. A matching replay returns its recorded result. The same ID with a different payload is rejected.
3. It checks the purchasable catalog, cash, research, territory footprint and refund provenance using durable state.
4. The townhall-cell simulation validates the living Mayor's identity/position, terrain, footprint occupancy and living-player obstruction. A placement reserves its collider/occupancy but is hidden from building views. The builder is temporarily held stationary and protected from damage/handoff while the operation resolves. Demolition retains the old collider until committed. Other players and the simulation continue running.
5. The conductor atomically commits the debit/building or removal/refund together with the request result, then acknowledges. Failed commands record a rejection without spending or reserving a permanent footprint.
6. A committed projection is published to the home cell. Independent periodic reconciliation also reads active durable records under row locks, waits for any prepare transaction to resolve, and applies them through the simulation control queue. This recovers lost applications/acknowledgements and reconstructs buildings after cell restart.

Database IO occurs in asynchronous coordination/reconciliation tasks, never inside the per-frame simulation mutation loop. The row lock spans preparation and commit; the simulation world lock does not span network/database IO.

Reservations are resolved by a newer committed revision or an authoritative locked read initiated after preparation. An earlier in-flight read cannot incorrectly cancel a pending placement. If a conductor fails before commit, its transaction rolls back; reconciliation releases the reservation and restores the previous state. If it fails after commit, reconciliation applies the committed result and a retry returns the original outcome. A restarted cell requires a fresh authoritative read to bootstrap a town; an unsolicited old apply notification alone cannot recreate it. End notifications/town-directory reconciliation retire assets, and tombstones reject later projections for retired games.

Income progress and request results are stored with each successful economic commit. The initial implementation uses a per-game JSON record and a serialized coordinator; large ledgers/high player counts will warrant normalized command/building tables and more parallel coordination while preserving the same contracts.

## Routing and territory limits

Known routing-cell assignments are sticky during conductor uptime: registering another cell does not reassign an already-routed region. Active durable home-cell bindings are restored before routing becomes available after conductor restart. Keep configured cell IDs stable. A missing pinned owner yields `authority_unavailable`, rather than creating a second copy under a different owner. General live reassignment/migration remains Phase 6.

Territory containment bounds construction authority; it does not implement neighboring-server entity replication. Cross-boundary visibility/collision/targeting and projectile migration remain Phase 6. The current simulation's player-obstruction check covers players on the authoritative server. The territory itself is a permanent player-facing rule, not a promise to permit unlimited construction later.

## Browser protocol

During an authenticated `/play` session:

```json
{"type":"place_building","request_id":"<UUID>","building_type":"wall_research","origin_tile":{"x":13,"y":16}}
{"type":"demolish_building","request_id":"<another UUID>","building_id":"<server-issued UUID>"}
```

Optional `v` is accepted by the gateway. Extra command fields such as price, cash, ownership, or refund are rejected. The acting player is always derived from the session. Anonymous debug sessions cannot construct.

Replies:

```json
{"type":"construction_result","result":{"request_id":"<UUID>","error":null,"building_id":"<UUID>","cash":50000,"refund":0}}
{"type":"economy_state","cash":50000,"territory":{"min_tile":{"x":0,"y":0},"size_tiles":{"x":32,"y":32}},"constructible_types":["wall_research","wall_factory"],"demolition_refund_percent":25}
```

`error` is a structured snake_case rejection, for example `not_alive`, `outside_territory`, `missing_research`, `insufficient_cash`, `occupied`, `player_obstruction`, `not_owned`, `town_hall_protected`, `request_id_conflict`, `authority_unavailable`, `invalid_command`, or `service_unavailable`. A lost reply/service timeout has an uncertain outcome: retry with the **same request ID and payload**. An explicit recorded rejection is final for that ID; a new attempt after changing circumstances uses a new ID. Result cash is the historical balance at that request's outcome; `economy_state` and snapshots provide the latest balance.

Normal state snapshots include `construction: {territory, can_construct, reason}` and authoritative `self_state.wallet.cash`. `can_construct` covers actor eligibility, not the validity/affordability of every possible placement. Factory production remains `null` in this phase.

Shared contracts and subjects are centralized in `twz-shared/src/economy.rs`: `twz.economy.request`, `twz.economy.world`, and server-specific prepare/apply subjects. Internal NATS and cell backend sockets retain their existing trusted-service boundary.

## Configuration and deployment

Configure the **conductor** with optional server-side development tuning:

```dotenv
TWZ_MAYOR_INCOME_AMOUNT=100
TWZ_MAYOR_INCOME_INTERVAL_MS=3000
```

The amount must fit the checked wallet limit and the interval must be positive. Defaults are the normal gameplay values above. These are not `VITE_*` settings. The CI live construction fixture uses 55,000 per 1,000 ms to exercise normal catalog prices quickly.

Rebuild/restart conductor, gateway and cells together, preserving cell IDs. The conductor applies migrations 004 and 005 automatically using `DATABASE_URL`. The existing React client remains protocol-compatible; construction controls arrive in Phase 3c.

## Verification

Run `cargo fmt --all -- --check`, `cargo check --workspace --locked`, and `cargo test --workspace --locked`.

The standard tests cover eligible-time accounting, negative/edge territory bounds, research gating, original-price refunds, command-field rejection, actor/overlap checks, forecourt access, reservation rollback/stale reads, recovered cell state, terminal cleanup, and stable routing.

With isolated PostgreSQL/NATS, repository tests cover transaction rollback, durable replay after reopening, stale checkpoint override, stale connection fencing, demolition persistence, ended-game exclusion, and concurrent spending. The live gateway construction test covers the real income/research/factory/refund/replay/resume path. `.github/scripts/integration.sh` runs all ignored tests in the required order and CI supplies accelerated income for the live fixture. Broader UI/fault-injection acceptance is Phase 3d.
