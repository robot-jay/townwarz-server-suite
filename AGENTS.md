# Townwarz Server Suite

## Workspace

- This is a Cargo workspace: `twz-conductor` handles cell routing and game/economy coordination; `twz-gateway` is the client WebSocket relay; `twz-cellserver` owns authoritative simulation; `twz-shared` contains cross-service contracts. `twz-migrate` is a separate deployment command, and `twz-schema` holds read-only schema compatibility checks. Container lifecycles belong to the deployment platform, not the conductor.
- The React/TypeScript/Vite browser client is an ordinary monorepo directory at `twz-webclient/`, not a Cargo member or Git submodule. Run npm commands there, or use `npm --prefix twz-webclient ...` from the root. Its `.env` is separate from the Rust services' root `.env`.
- Keep wire contracts and NATS subjects centralized in `twz-shared/src/lib.rs`. Gateway and cell-server handoff uses `handoff.<server>.req`, `handoff.<server>.ack`, and `switch.<player-id>`; town creation uses `twz.town.create` and cell spawn uses `twz.cell.<x>.<y>.spawn`.
- A cell server must register with the conductor before the gateway can route a client. Its advertised address must be reachable from the gateway; use `--advertise twz-cell-A:3001`-style internal container DNS names when services are managed externally.

## Verification

- Run `cargo check --workspace` for a focused compile check, `cargo test --workspace` for the standard workspace tests, and `cargo fmt --all -- --check` to detect formatting drift.
- Client checks: `npm --prefix twz-webclient ci`, `npm --prefix twz-webclient test`, and `npm --prefix twz-webclient run build` (includes strict TypeScript checking).
- Client browser checks: install Chromium with Playwright, then `npm --prefix twz-webclient run test:browser`. The suite starts its own Vite server on port 5176 and mocks Auth0/catalog/WebSocket responses; it needs no live Rust services. CI installs Chromium and runs these tests automatically. Browser tests/configuration are included in the client's TypeScript check.
- The root `.github/workflows/ci.yml` runs client and server checks on PR updates and pushes to `master`. Service-backed Rust tests are ignored in the default suite; `.github/scripts/integration.sh` runs them against isolated PostgreSQL/NATS, then a real conductor and two cells. Do not run mock NATS responders alongside the real conductor on the same subjects.
- Run `cargo run --locked -p twz-migrate -- up` explicitly before application/database integration tests. Runtime repository connections must never perform DDL or initialize migration history. Migration SQL lives in `twz-migrate/migrations/`; schema 6/reader 1 adds versioned deployment history and read-only compatibility validation. Deployment tests use isolated schemas and the runtime-permission fixture requires test-role administration privileges.
- Phase 3 acceptance (`phase3_acceptance` filter) owns/restarts its conductor/cell children and uses an isolated test-database commit barrier. Run it after mock tests and before any long-lived conductor on the same NATS. It needs built service binaries (`TEST_SERVICE_BIN_DIR`, default `target/debug`), client npm dependencies and Chromium. Its browser fixture uses real game/catalog traffic with signed fixture authentication; only external Auth0 login UI is stubbed. See `PHASE3D.md`.

## Local Integration

- NATS is required by every service. Start it before the applications: `nats-server -p 4222`.
- With PostgreSQL available, run the deployment migration command before starting the conductor. Use deployment credentials for migration and runtime credentials for the conductor in deployed environments; both must target the same schema/search path.
- Start the conductor: `cargo run -p twz-conductor -- --bind 0.0.0.0:7000`.
- Start cells with distinct IDs and ports, for example: `cargo run -p twz-cellserver -- --id A --bind 127.0.0.1:3001 --conductor http://127.0.0.1:7000 --nats nats://127.0.0.1:4222` and the equivalent `B` process on `:3002`.
- Start the gateway after cells register: `cargo run -p twz-gateway -- --bind 0.0.0.0:8081 --conductor http://127.0.0.1:7000 --nats nats://127.0.0.1:4222`.
- Start the browser client: `npm --prefix twz-webclient run dev`; Vite serves it at `http://localhost:5173` and connects to the gateway's `/play` endpoint.
- `debug-client.html` is a static manual client, but it hardcodes `ws://localhost:8080/ws`; align it with the local gateway bind port (`:8081`) or run the gateway on `:8080` before using it.

## Deployment and container packaging

- The platform manages independent migration, conductor, gateway, cell, NATS and PostgreSQL workloads. No Docker daemon/socket or supervisor config is used by the conductor. Configure gateway Auth0 values directly on the gateway and preserve cell IDs/advertised addresses.
- Build images from the workspace root with `docker build -t twz-conductor:local -f twz-conductor/Dockerfile .`, equivalently for gateway/cells and the one-shot `twz-migrate` image. Gate application rollout on `twz-migrate up` exit status 0; never add migrations back to application entrypoints. See `DEPLOYMENT.md`.
- Container Dockerfiles use Rust `1.85` and build from the workspace root; do not use an individual crate directory as the Docker build context.
