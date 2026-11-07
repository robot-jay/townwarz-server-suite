# Independently managed services and deployment migrations

Container/process lifecycle belongs to the deployment platform (including Azure), not the conductor. Schema changes belong to a release migration job, not application startup.

## Workload boundaries

| Workload | Responsibility and configuration |
| --- | --- |
| `twz-migrate` | One-shot release operation. Needs PostgreSQL connectivity and the deployment database identity. No NATS, service listeners, Docker daemon or application containers are needed. |
| `twz-conductor` | Cell directory/routing, town/game lifecycle, reconnect/expiry, economic coordination and application persistence. Needs runtime `DATABASE_URL`, NATS URL, bind address, and optional Mayor income tuning. |
| `twz-gateway` | Authenticated browser WebSockets and relay. Configure its conductor/NATS addresses and `AUTH0_DOMAIN`/`AUTH0_CLIENT_ID` directly. These values are no longer forwarded by the conductor. |
| `twz-cellserver` | Authoritative simulation. Configure a stable `--id`, bind address, reachable `--advertise` address, conductor URL and NATS URL. It does not need database credentials. |
| PostgreSQL / NATS | Independently provisioned dependencies with their own storage, connectivity and lifecycle. |
| React client | Independently built static assets with public `VITE_*` configuration. |

The conductor has no Docker client, network/container creation code, supervisor configuration, `--supervise`, or supervisor `--config` mode. Its image runs as the existing unprivileged user and does not require Docker-socket access. The service Dockerfiles remain packaging artifacts; the platform decides how many containers exist and when to restart them.

Keep cell IDs stable. Replacing a process under the same ID and allowing it to re-register is different from reassigning logical world ownership. Known routing assignments and durable home-cell bindings retain their existing semantics; general live reassignment and multi-active conductor operation are not introduced by removing supervision.

## Release sequence

```text
Build/test the release's application and migration artifacts
    → Run twz-migrate up using the deployment database identity
    → Require exit status 0
    → Deploy/update independently managed application containers
    → Verify service startup/health and publish the matching frontend
```

Do not put `twz-migrate up` in each application's entrypoint or per-replica startup command. Run it as a separately gated deployment step. Duplicate migration jobs serialize safely, but the intended unit is one release operation.

The database itself and the chosen application schema must already exist. Migration and runtime identities must resolve the **same application schema/search_path**; the local/default schema is `public`. The runner creates application objects in that schema, not a database server, database, login roles or a cloud deployment.

## Migration command

From the repository root, with `DATABASE_URL` supplied to that process:

```sh
cargo run --locked -p twz-migrate -- up
cargo run --locked -p twz-migrate -- check
```

- **`up`** applies pending migrations, verifies the resulting contract, then exits. A failure returns nonzero and must stop the application rollout.
- **`check`** only reads schema/history/column metadata and exits successfully if this application's reader contract is supported. It does not create or repair anything and may run with runtime permissions.
- No down/reset/refresh command is exposed. The migrations' `down` implementation also rejects rollback.

`twz-migrate/Dockerfile` packages the same command. Its default container command is `up`; `check` and `--help` are explicit alternatives. For example, with a deployment `DATABASE_URL` reachable **from the container**:

```sh
docker build -t twz-migrate:local -f twz-migrate/Dockerfile .
docker run --rm -e DATABASE_URL twz-migrate:local up
docker run --rm -e DATABASE_URL twz-migrate:local check
```

There is no requirement to run NATS or any game service while this job executes. Container networking and credential delivery are configured by the caller/platform.

## Existing databases and history

The original SQL files moved intact to `twz-migrate/migrations/`:

1. Games/users and hall indexes.
2. Connection/checkpoint lifecycle.
3. Expiry acknowledgement.
4. Durable economy JSON.
5. Sequence-backed participation ordering.
6. New schema compatibility metadata.

SeaORM's migration framework records applied names/timestamps in `seaql_migrations`. An existing database from the former conductor-managed scheme has no such history: the first deployment run actually executes the original idempotent SQL before recording it. It does not blindly baseline based on a table name, erase player/game history, or replace checkpoints/economic JSON. Subsequent runs skip applied migrations rather than repeatedly executing all scripts.

The runner takes transaction-scoped advisory locks `7489220` (migration) then `7489221` (allocation) **before ledger creation**, retaining the legacy allocation/sequence protection. Pending migrations, history records and compatibility validation share a release transaction; failed schema validation rolls back its ledger and transactional DDL. PostgreSQL sequence gaps retain normal PostgreSQL semantics. Applied migration names and scripts are release history: add a new migration rather than changing an applied one.

## Runtime compatibility and startup

`twz-schema` is a small read-only contract library shared by the conductor and migration runner. The conductor depends on this library, not on the migrator at runtime.

Current schema contract: **schema 6, reader 1**. `twz_schema_metadata` records `schema_version` and `minimum_reader_version`; startup also verifies the required migration history and real application column types.

- An empty, missing, old or drifted schema causes the conductor to fail with an instruction to run deployment migrations.
- An unmigrated conductor performs no DDL and fails before opening HTTP/NATS request handlers.
- A newer additive schema can support an older application if it retains the required history/columns and its minimum reader remains compatible.
- A breaking migration must raise the minimum reader requirement. Older binaries reject that schema at startup; drain/replace existing old readers before a destructive contract migration.

Use expand/contract changes for rolling releases: add compatible schema first, deploy code, and remove obsolete schema in a later release. Application rollback does not run database down migrations. Roll back application code only when its reader remains compatible; otherwise deploy a forward corrective release.

`GET /health` on the conductor becomes available after schema validation, initial NATS connection and handler initialization. It does not depend on having registered cells, avoiding a startup-probe dependency cycle. It is a process/startup health endpoint, not a continuous database/NATS availability guarantee.

## Separate database identities

Use the same environment-variable name, `DATABASE_URL`, with **different workload-scoped credentials**:

- **Migration job:** schema creation/alteration and ownership privileges needed for release migrations.
- **Conductor runtime:** application DML, sequence usage and read-only migration/schema metadata access. No ownership or CREATE/ALTER privileges on application objects.
- **Gateway/cells:** no production PostgreSQL credentials.

An illustrative grant set, substituting the actual existing schema and runtime role:

```sql
GRANT USAGE ON SCHEMA app_schema TO townwarz_runtime;
GRANT SELECT, INSERT, UPDATE, DELETE
    ON app_schema.twz_users, app_schema.twz_games TO townwarz_runtime;
GRANT SELECT ON app_schema.seaql_migrations, app_schema.twz_schema_metadata
    TO townwarz_runtime;
GRANT USAGE, SELECT ON SEQUENCE app_schema.twz_participation_order_seq
    TO townwarz_runtime;
```

Also arrange database connection permission and the same search path for both identities. If an environment grants schema CREATE broadly through `PUBLIC` (as some PostgreSQL `public` schemas do), database provisioning must restrict that grant or use a restricted application schema. The migration runner does not change cluster-wide role policy or guess runtime role names. Future migrations must include any additional runtime grants needed by their new objects.

Local development may use one owner identity for both processes; it still runs the migration command explicitly. Production credentials for the migration job should not be supplied to the conductor. `AUTH0_DOMAIN`/`AUTH0_CLIENT_ID` are gateway configuration; no client secret is introduced.

## Local development and CI

Create/configure PostgreSQL first, run `twz-migrate up`, then start NATS, conductor, cells and gateway. Existing databases need the new history/metadata adoption step before starting this conductor version.

The root integration workflow builds `twz-migrate` alongside the services and `.github/scripts/integration.sh` runs `up`/`check` before repository/service tests. Self-managed Phase 3 acceptance suites likewise execute the migration command as a deployment gate before starting their conductor children. Runtime restarts only check compatibility.

Additional opt-in PostgreSQL tests cover:

- Empty database CLI initialization and read-only failure before initialization.
- Legacy profiles, checkpoints, expiry acknowledgement and economic-state preservation.
- Repeat-run history stability and simultaneous runner serialization.
- Failed-release rollback when a legacy schema has incompatible column types.
- Additive future-schema compatibility and breaking-reader rejection.
- Actual repository CRUD/economic/reconnect operations under a restricted login whose table creation and alteration attempts fail.
- A real conductor process failing fast on an empty schema, without creating tables or waiting for NATS.

The schema/permission fixtures use unique schemas in `TEST_DATABASE_URL`; the restricted-login test requires test-role administration permission. Use an isolated test database/administrative fixture identity. Ordinary tests require no running database; deployment and service-backed tests remain opt-in.

Verification for this change passed **38 standard Rust tests** and **21 service-backed tests**, including the real-stack Chromium flow, legacy adoption, concurrent migration runners, failed-release rollback and restricted runtime permissions. The migration and conductor images also built with the existing Rust 1.85 container toolchain. A container smoke test ran the migration image's `up`/`check`, then reached the standalone conductor's `/health` using externally started NATS; the conductor ran as `65532:65532` with no mounts or Docker socket.
