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

Game-entry metadata uses `active_game_summary`, a read-only five-column ORM projection with no checkpoint retrieval or session-token update. The authenticated check endpoint runs normal expiry housekeeping before this lookup. Targeted abandonment filters the subject's active IDs against the displayed game ID before invoking the existing terminal/promotion operation.

When there is no active personal game, `expired_game_notice` projects the latest participation's end metadata, not its private checkpoint. It reports only an unacknowledged timeout, with original-town availability and future per-role vacancies. `acknowledge_expiry` uses an authenticated-subject/game-ID scoped ORM update. Migration `003_expiry_notice.sql` stores the acknowledgement timestamp; existing history remains intact.

The repository owns a SQLx connection pool (minimum 1, maximum 10, eight-second connect/acquire timeouts). Cloned repositories share that pool; independent instances can use separate pools against the same database. Statement logging is disabled by default. Driver TLS behavior follows the PostgreSQL connection URL and SQLx defaults.

## Why some SQL remains

These operations intentionally remain explicit and atomic:

- Transaction-scoped advisory lock `7489221` serializes allocation/resume/expiry across connections and processes.
- Hall allocation and prior per-site claim timestamp lookup execute under that lock, with the existing active-hall unique index as a backstop. Fresh games randomly choose an available site from an initial 16-hall (4x4) pool. The pool grows to at least live/reserved claim count plus two, ensuring alternatives remain available. An account's most recent hall is excluded for a fresh game; reconnects retain their original assignment. Unexpired disconnect reservations remain excluded.
- Resume selects its complete record `FOR UPDATE`, checks the old token, updates it, and builds its admission from that locked row. It does not reread the record after commit.
- Heartbeat/disconnect update only a row with the matching live connection token. Missing checkpoints preserve the stored checkpoint via `COALESCE`.
- Terminal updates use `UPDATE ... RETURNING`, so repeated/stale requests return no affected town and do not generate a second lifecycle notification.
- Expiry marks stale connections and conditionally ends expired games in one transaction. It returns the games actually ended, avoiding a read-IDs-then-end race with resumed connections.

All runtime values in these statements are bound parameters. The standalone SQL files document the invariants they protect.

## Schema and configuration

Use the existing workspace-root `.env` and `DATABASE_URL`. The ORM refactor itself required no schema/data change. Startup applies the idempotent `001_games.sql`, `002_reconnect.sql`, and `003_expiry_notice.sql` files transactionally, protected by a separate migration advisory lock. The third migration adds the later acknowledge-once expiry notice. Entity definitions map the schema; they do not automatically recreate tables or synchronize/drop columns.

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

Repository tests no longer require NATS. They cover profile upsert, existing-data preservation on reopening, checkpoint/inventory round trips, grace reservation, stale heartbeat/disconnect/quit rejection, idempotent end, timeout, history retention, and simultaneous hall/account/resume claims using independent pools. Random-allocation checks cover avoiding immediate repeats, valid generated sites, per-site generation history, full-pool expansion and reserved-hall exclusion. Fixtures are scoped to unique test users and cleaned up. The normal gateway live-cell integration still exercises the application service and NATS event path with PostgreSQL behind it.
