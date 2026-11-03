# Returning after reconnect grace expires

## Agreed behavior

- A returning player whose latest participation ended with `timeout` is told that the **15-minute reconnect grace elapsed because they were away too long**. The entry dialog names their former town and does not load a simulation.
- The notice is acknowledged **once per expired game**, stored server-side so it stays dismissed across reloads, browsers and logins.
- A former town is identified by its original **town ID**, never by a physical hall slot. A different town reusing the hall is not offered as the player's old town.
- Inactive former towns cannot be selected. Continuing opens normal new-game setup, using randomized hall allocation.
- The future roster has **one Mayor and four Commandos**. Returning to a surviving town after expiry will be a **fresh participation**: new identity, zero personal cash, empty inventory, full role health and a townhall spawn. The surviving town retains its own state.
- This task implements expiry entry and its future availability contract, not multi-member towns, promotions or joining. Until those systems exist, joining actions are explicitly unavailable. Today's expired solo-mayor town is inactive.

## Server flow

`twz.game.check` first performs normal expiry housekeeping. A live personal game takes precedence; otherwise the repository examines the latest participation. It returns a notice only if that latest row is ended for `timeout` and has no acknowledgement. It does not search backward for an older timeout after a later game, quit or Orb defeat.

`GameLookupResponse.expired_game` carries `ExpiredGameNotice`:

```text
player_id, town_id, town_name, expired_at_ms, grace_period_seconds
former_town:
  is_active
  rejoin_supported
  state_on_join: fresh
  roles[]:
    role, capacity, occupied, vacancies, can_join, unavailable_reason
```

The current roster projection checks active game records by the original town ID. Existing schema ties town lifetime to its sole mayor; independent town/roster records must replace this projection when membership/succession is implemented. `TownRejoinAvailability::pending_membership` preserves explicit per-role limits and vacancy counts, with `rejoin_supported=false` and `can_join=false`. No role is advertised as joinable merely because a capacity slot is empty.

The gateway translates lookup into `existing_game`, `grace_expired`, or `no_game`. A direct failed resume after timeout also performs lookup and returns the same expiry explanation. Expired checkpoints are never used to enter the simulation.

An authenticated initial `/play` message:

```json
{"type":"acknowledge_expiry","id_token":"<Auth0 ID token>","player_id":"<expired game UUID>"}
```

is forwarded to `twz.game.expiry.ack`. The repository updates only the matching authenticated subject's ended timeout row, setting `expiry_notice_ack_at` only if it is null. Acknowledgement is idempotent; an old or foreign ID cannot alter another game's state or dismiss another user's notice. The gateway responds `expiry_notice_ack` without claiming a lease or opening a cell-server socket.

## Client flow

The expiry panel explains the elapsed window and why the old game cannot continue. For an inactive town it offers no town/role selection. For the future surviving-town branch it displays explicit role vacancies with disabled joining controls and a clear membership-unavailable explanation. Starting a new game remains available.

The client waits for acknowledgement before opening fresh role/name setup. Failures keep the notice visible and allow retry. Merely viewing the dialog or signing out does not count as acknowledgement. Once acknowledged, the latest timeout history/checkpoint remains archived, but the same notice no longer appears.

## Deployment and checks

Migration `twz-conductor/migrations/003_expiry_notice.sql` adds nullable `expiry_notice_ack_at` and is applied automatically by the SeaORM repository. No new environment variables are needed. Update conductor, gateway and client together; restart cells as needed to re-register with a restarted conductor.

Verification includes standard workspace/contract tests, PostgreSQL notice scoping and persistence, original-town versus reused-hall identity, latest-game precedence, fresh-start state, and live gateway/cell checks for check/resume/ack flows. Browser smoke checks cover the explanation, inactive-town exclusion, unavailable role actions, acknowledgement failure/retry, persistence across reload, and fresh setup without entering the old simulation.

Initial local verification completed with workspace format/compile/tests, six client tests and production build, four PostgreSQL integration tests against an isolated temporary database, and a live gateway test using separate NATS/conductor ports and two cells. Temporary services and databases were removed afterward. An intermittent missing-latest-timeout assertion was later reproduced during Phase 3b verification: PostgreSQL's wall clock moved backward between participations. Migration `005_participation_order.sql` now supplies a monotonically allocated participation order for history lookup and previous-hall selection; the regression test explicitly moves an older participation's timestamp into the future to verify that wall-clock corrections cannot select it as latest.
