# Phase 3d — integration acceptance

Phase 3 is accepted: a Mayor can earn cash and purchase Wall Research followed by Wall Factory through the React UI, with authoritative spending, prerequisites, territory rules and recoverable outcomes. The behavior contract remains [ROADMAP.md](ROADMAP.md); implementation details are in [PHASE3B.md](PHASE3B.md) and [PHASE3C.md](PHASE3C.md).

## Acceptance coverage

| Requirement | Evidence |
| --- | --- |
| Earn → research → factory through the UI | `phase3_acceptance_browser_builds_through_real_services` runs Chromium against a real gateway, conductor, two cell processes, PostgreSQL and NATS. It also checks preview cancellation sends no purchase and research demolition retains the factory. |
| Spending/prerequisites/replay | Existing real-gateway construction test plus the competing-placement acceptance case verify research gating, catalog-field rejection, request-ID conflict, occupied-footprint rejection and successful replay. The competing test includes exact wallet accounting after income: only one debit is permitted. |
| Concurrency | Two placement intentions are submitted before either outcome is read; exactly one obtains the same footprint. PostgreSQL independent-connection spending tests cover row-lock serialization as well. |
| Refund provenance/rounding/replay | The real demolition handler receives a historical `price_paid=8003` fixture while the current catalog price is 5000. It refunds exactly 2000, once. Replays cannot credit again. Unit and UI tests additionally cover integer precision and displayed provenance. |
| Connected/dead/respawning income | The income acceptance case resumes a genuinely dead cell avatar, observes income before respawn and after actual respawn, and checks it against the gateway's durable cumulative eligible time. |
| Disconnect and partial progress | The live test verifies the durable balance/progress do not change during a disconnected interval. After reconnect it checks `old cash + floor((old progress + new connection elapsed)/interval) × amount`. The repository test uses an explicit nonzero fraction to verify the exact boundary. |
| Handoff | Actual NATS handoff adoption/acknowledgement and gateway backend switching preserve one connection-scoped income clock. Construction away from the home territory is rejected. The ordinary source freeze/crossing path remains covered by the existing live cell handoff test. |
| Before durable commit | A test-database trigger waits on a held PostgreSQL advisory lock after real cell preparation. The fixture kills its conductor process before commit, verifies no debit/building/result was committed, restarts the conductor, observes reservation release, and retries the original request exactly once. |
| Commit without application/acknowledgement | With the same barrier, the fixture kills the prepared cell and lets the client connection close before releasing the commit. It then restarts that cell, resumes with deliberately stale checkpoint cash, and verifies the durable building/debit and original replay result. |
| Applied result without client acknowledgement | A test-only WebSocket proxy drops the targeted construction result while forwarding real state. The test observes the building in simulation, reconnects, replays the exact request, and verifies unchanged cash/entity identity. |
| Demolition recovery | Restarting the cell after demolition and replaying the earlier successful purchase cannot recreate the removed entity. |
| Ended games and reused halls | After game end and cell restart, a late old projection cannot restore retired assets. A fixture participation then claims the same physical hall; old apply/cleanup messages cannot remove or rename its new claim. |
| UI controls and feedback | Five mocked Chromium scenarios cover catalog failure/retry, affordability/research/death/territory locks, preview/confirm/cancel, movement and firing suppression, original-price demolition, uncertain-request reload recovery, server errors and narrow-screen layout. |

## Test harness and boundaries

`twz-gateway/src/acceptance_tests.rs` contains four opt-in acceptance tests. They use the actual gateway handlers with the existing signed JWT/JWKS fixtures, and spawn/restart normal conductor and cell executables on allocated loopback ports. They require an isolated `TEST_DATABASE_URL` and `TEST_NATS_URL`, with no other conductor answering on that NATS instance.

The harness deliberately manipulates **fixture data** for deterministic scenarios: a fixed wallet for zero-income failure cases, a dead/stale movement checkpoint, a historical paid price, and an explicitly reused hall site. Income/handoff assertions before the spending fixture use real connected income. No application API for arbitrary cash, teleportation or fault injection is added.

The pre-commit barrier is a PostgreSQL trigger/function installed only in the isolated test database and keyed to one generated request UUID. The fixture observes `pg_locks`, kills only its own child, then releases/removes the barrier. This tests real transaction rollback/commit and real recovery tasks rather than replacing the economy implementation with a mock. The database user must be able to create fixture triggers/functions and inspect lock state (CI uses its isolated PostgreSQL owner).

The live browser test is `twz-webclient/tests/phase3-live.spec.ts`. Auth0's external login UI is stubbed, but its signed test ID token is verified by the gateway. The catalog fetch, WebSocket gameplay, economic coordinator, simulation, persistence and CORS path are real. The Rust wrapper checks the resulting durable infrastructure and request count after the browser completes.

The harness reaps its service children and aborts its gateway/listener tasks on completion or failure. PostgreSQL/NATS remain owned by the caller/CI job. It writes service logs under `target/ci-logs/phase3-*`; Playwright traces/screenshots use `twz-webclient/test-results/live/`. Mock browser diagnostics use a separate `test-results/mocked/` folder.

## Recovery fixes found during acceptance

1. **Cell re-registration:** Cells now re-send their original registration when a heartbeat receives `404 unknown_server`. A restarted conductor can recover the live cell directory with stable IDs/addresses, without requiring those cells to be restarted. The pre-commit process-crash test verifies this path.
2. **Away-from-home rejection:** A valid, connected Mayor whose avatar is hosted on another cell now receives `outside_territory` from the home construction authority rather than the misleading `inactive_game`. Lifecycle/token checks still happen in the conductor before spatial preparation.

These fixes require updating the cell binary. No new migration or public gameplay command is introduced by Phase 3d.

## Running the suite

Standard checks from the repository root:

```sh
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --workspace --locked
npm --prefix twz-webclient test
npm --prefix twz-webclient run build
npm --prefix twz-webclient run test:browser
```

Install Chromium first using Playwright from `twz-webclient/` (`npx playwright install chromium`). For service-backed checks, build conductor/cells and provide isolated PostgreSQL/NATS plus the endpoints used by `.github/scripts/integration.sh`. The script runs three stages in order:

1. Repository and mock-service/handoff tests, excluding live fixtures.
2. `cargo test --workspace --locked phase3_acceptance -- --ignored --test-threads=1 --nocapture`: self-managed recovery/browser fixtures, before any other conductor starts.
3. The existing long-lived conductor/two-cell gateway tests.

`TEST_SERVICE_BIN_DIR` optionally points to the directory containing the built conductor/cell executables; the default is `target/debug`. The acceptance browser wrapper supplies `TEST_REAL_GAME_WS_URL`, `TEST_REAL_ID_TOKEN`, and an allocated `TEST_BROWSER_PORT` to Playwright automatically. Normal `npm run test:browser` selects the five mocked UI scenarios; the wrapper selects the one real-stack scenario. No actual Auth0 tenant or secret is required.

GitHub Actions installs npm dependencies and Chromium in the integration job, runs these stages, and uploads service logs and browser diagnostics on failure. Standard Rust tests keep the service-backed fixtures ignored, so plain `cargo test` still needs no PostgreSQL/NATS.

## Accepted milestone limits and next phase

Final local verification passed: **37 standard Rust tests**, **14 service-backed Rust tests** (including the four new acceptance wrappers), **15 client unit/regression tests**, **five mocked Chromium scenarios**, and **one real-stack Chromium scenario** launched by its Rust wrapper. Rust formatting/compile checks, TypeScript/production build, Actionlint and ShellCheck also passed. Isolated service children were reaped and the temporary verification database was removed after the run.

- Construction is the Wall Research/Factory branch in finite 32×32-tile town territories. Production, pickups, inventory controls and Wall deployment start in **Phase 4**.
- Territory containment does not supply neighboring-cell visibility/collision, projectile migration, or live cell reassignment; those remain Phase 6. Handoff acceptance exercises the current player-state transport without claiming those broader world guarantees.
- Durable economic cash is authoritative; movement still uses periodic checkpoints. An abruptly lost gateway can lose the last uncommitted eligible-time sample, rather than earning disconnected-time catch-up.
- The initial economy uses a per-game JSON request ledger and serialized coordinator. Future production/combat work must extend that consistency boundary rather than separately mutating transient wallets.
