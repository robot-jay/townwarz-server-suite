# Townwarz Server Suite

## Workspace

- This is a Cargo workspace: `twz-conductor` assigns cells and supervises Docker services; `twz-gateway` is the client WebSocket relay; `twz-cellserver` owns authoritative simulation; `twz-shared` contains the cross-service HTTP/NATS data contracts.
- Keep wire contracts and NATS subjects centralized in `twz-shared/src/lib.rs`. Gateway and cell-server handoff uses `handoff.<server>.req`, `handoff.<server>.ack`, and `switch.<player-id>`; town creation uses `twz.town.create` and cell spawn uses `twz.cell.<x>.<y>.spawn`.
- A cell server must register with the conductor before the gateway can route a client. Its advertised address must be reachable from the gateway; use `--advertise twz-cell-A:3001`-style container DNS names when running under the Docker supervisor.

## Verification

- Run `cargo check --workspace` for a focused compile check, `cargo test --workspace` for the full workspace, and `cargo fmt --all -- --check` to detect formatting drift. There are currently no authored Rust tests, so `cargo test` mainly verifies compilation and empty test harnesses.
- The repository is not currently rustfmt-clean; run `cargo fmt --all` when intentionally formatting Rust changes.

## Local Integration

- NATS is required by every service. Start it before the applications: `nats-server -p 4222`.
- Start the conductor: `cargo run -p twz-conductor -- --bind 0.0.0.0:7000`.
- Start cells with distinct IDs and ports, for example: `cargo run -p twz-cellserver -- --id A --bind 127.0.0.1:3001 --conductor http://127.0.0.1:7000 --nats nats://127.0.0.1:4222` and the equivalent `B` process on `:3002`.
- Start the gateway after cells register: `cargo run -p twz-gateway -- --bind 0.0.0.0:8081 --conductor http://127.0.0.1:7000 --nats nats://127.0.0.1:4222`.
- `debug-client.html` is a static manual client, but it hardcodes `ws://localhost:8080/ws`; align it with the local gateway bind port (`:8081`) or run the gateway on `:8080` before using it.

## Docker Supervision

- `twz-conductor/conductor.toml` is the source of truth for supervised NATS, gateway, and cell-server image names, ports, arguments, and the two default cells.
- Build images from the workspace root with `docker build -t twz-conductor:local -f twz-conductor/Dockerfile .`, and equivalently for `twz-gateway` and `twz-cellserver`. The conductor's `--supervise --config <path>` mode requires access to the Docker daemon and creates/maintains the configured `twz_net` services.
- Container Dockerfiles use Rust `1.85` and build from the workspace root; do not use an individual crate directory as the Docker build context.
