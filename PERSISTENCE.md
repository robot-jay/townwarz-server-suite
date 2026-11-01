# Conductor persistence

The conductor uses **SeaORM 1.1 / SQLx PostgreSQL** behind `GameRepository`.

```text
NATS commands
    -> TownRegistry: validation, command flow, lifecycle notifications
        -> GameRepository: database transactions and domain records
            -> SeaORM entities / PostgreSQL
```

## Layout

- `twz-conductor/src/persistence/entities/`: mappings for existing `twz_users` and `twz_games` tables, including their user/game relationship, JSONB checkpoints and timezone-aware timestamps.
- `persistence/repository.rs`: pooled connection setup, domain conversion, profile upserts, inserts, projections, connection claims, termination and expiry.
- `persistence/sql/`: focused parameterized SQL for PostgreSQL-specific locking/allocation and compare-and-update operations.
- `persistence/tests.rs`: opt-in real PostgreSQL lifecycle and concurrency checks.

`TownRegistry` contains no SQL, ORM entities, database connections or database mutex. Repository results contain `Town`, `PlayerState`, connection IDs or affected domain records. NATS publishing stays in the application service, after database operations commit.

## ORM operations

SeaORM handles profile upsert, new game insertion, active-account checks, active town/subject queries, and updating an already row-locked resume claim. Directory queries project only town JSON; polling does not retrieve everyone's private checkpoints. Timestamps continue to use PostgreSQL's current timestamp/defaults.

The repository owns a SQLx connection pool (minimum 1, maximum 10, eight-second connect/acquire timeouts). Cloned repositories share that pool; independent instances can use separate pools against the same database. Statement logging is disabled by default. Driver TLS behavior follows the PostgreSQL connection URL and SQLx defaults.

## Why some SQL remains

These operations intentionally remain explicit and atomic:

- Transaction-scoped advisory lock `7489221` serializes allocation/resume/expiry across connections and processes.
- Hall allocation and prior per-site claim timestamp lookup execute under that lock, with the existing active-hall unique index as a backstop.
- Resume selects its complete record `FOR UPDATE`, checks the old token, updates it, and builds its admission from that locked row. It does not reread the record after commit.
- Heartbeat/disconnect update only a row with the matching live connection token. Missing checkpoints preserve the stored checkpoint via `COALESCE`.
- Terminal updates use `UPDATE ... RETURNING`, so repeated/stale requests return no affected town and do not generate a second lifecycle notification.
- Expiry marks stale connections and conditionally ends expired games in one transaction. It returns the games actually ended, avoiding a read-IDs-then-end race with resumed connections.

All runtime values in these statements are bound parameters. The standalone SQL files document the invariants they protect.

## Schema and configuration

Use the existing workspace-root `.env` and `DATABASE_URL`. No new environment keys or schema/data migration are required for the ORM refactor. The repository applies the unchanged idempotent `001_games.sql` and `002_reconnect.sql` files transactionally at startup, protected by a separate migration advisory lock. Entity definitions map the existing schema; they do not automatically recreate tables or synchronize/drop columns.

Gameplay and wire behavior remain the same: player-owned wallets, checkpoints, 15-minute reconnect grace, stale-token fencing, explicit quit, promotion hooks, and terminal Orb handling.

## Verification

```sh
cargo check --workspace
cargo test --workspace
```

For an isolated development/test database:

```powershell
$env:TEST_DATABASE_URL = 'postgres://postgres:your-password@127.0.0.1:5432/townwarz_test'
cargo test --workspace postgres_ -- --ignored --nocapture
```

Repository tests no longer require NATS. They cover profile upsert, existing-data preservation on reopening, checkpoint/inventory round trips, grace reservation, stale heartbeat/disconnect/quit rejection, idempotent end, claim reuse/order, timeout, history retention, and simultaneous hall/account/resume claims using independent pools. Fixtures are scoped to unique test users and cleaned up. The normal gateway live-cell integration still exercises the application service and NATS event path with PostgreSQL behind it.
