# Phase 1: gameplay domain and authoritative player state

## Decisions

- Cash belongs to individual player wallets, initially zero.
- Combat death clears all six inventory slots and retains cash. New games create empty wallets/inventories. No resume behavior changes.
- Allies share a non-null town ID. Distinct townless debug players are enemies, not a shared faction. A player cannot damage themselves through their own projectile.
- Unfinished content retains stable IDs and known legacy data with `unavailable` status. No effects are invented for Hospital, House, Cloak, DFG, etc.
- Bazooka `rocket` ammunition and `homing_missile` are separate items. The legacy `MilfMissileResearch`/`MilfMissileFactory`/`MilfMissile` names map to canonical `homing_missile_*` identifiers. Bazooka use remains unavailable.
- Team cash sharing and assists are planned for individual wallets. Percentages, recipient eligibility, killer inclusion, assist thresholds/windows, and payout timing remain undecided.

## Definitions versus instances

`twz-shared/src/domain/` is the authoritative definition/contract location:

- `catalog.rs`: 28 building definitions, 13 item definitions, four deployment definitions. Named snake_case identifiers, `DOMAIN_VERSION = 1`, legacy prices/prerequisites, known one-output recipes, health/destructibility, geometry metadata and timing/targeting parameters.
- `entities.rs`: explicit building, item, and deployed-object instances, lifecycle ownership, provenance, health, production states, and six-slot inventories.
- `player.rs`: game identity, Mayor/Commando roles, checked wallet operations, inventory and cooldown state.
- `combat.rs`: immutable projectile attribution, town allegiance, defeat/contribution records, and configurable future reward policies.

`specified` means the branch has a known baseline definition, not that it is constructible or deployed by this phase. `unavailable` means additional gameplay design is required. Unspecified recipe durations and deployment geometry are null rather than guessed. Original normal-building reservation is 3x3 tiles with shorter 90px colliders for townhalls and the four implemented legacy factories; other legacy colliders are 144px. Visual bounds and canonical deployment sizes await Phase 2. These definitions do not replace the current single terrain-tile townhall yet.

Gateway `GET /catalog` exposes the read-only catalog and version. The client owns no price or recipe values. Missing gameplay commands cannot be enabled by changing client-side flags.

## Authoritative player state

Every simulated player has `PlayerGameState` containing:

```text
identity: game_id, town_id?, role?
wallet: cash
inventory: exactly six optional ItemInstance values
cooldowns: primary_fire_ready_at_ms, item_use_ready_at_ms
```

The current game record is keyed by its fresh player UUID, so `game_id` is initialized to that UUID. It is explicitly modeled separately from town identity and authenticated account identity, allowing future team members to have independent wallets/sessions. Anonymous debug identities have null town and role. No Auth0 token/profile data is put in public simulation snapshots.

The gateway supplies identity in the trusted backend `set_pid` message. Cells retain it as player state rather than inferring membership by scanning town labels or hashing UUIDs into teams. Normal gameplay input cannot set wallet, inventory, role, or town.

Player spawn/despawn, handoff adoption/acknowledgement, death, and gameplay state changes are processed by the simulation loop's control/input channels. PostgreSQL still stores user/game lifecycle records; it is not read or written each frame. Wallet/inventory are transient active-game state in this phase; no new database migration is needed.

## Handoffs

`PlayerState` carries the complete domain state, home spawn, health/respawn timer, and damage contributions. The receiving cell adopts it without creating fresh economic state. The later gateway WebSocket attachment keeps the adopted wallet/inventory/cooldowns. Repeated adoption of the same game acknowledges the existing player without rolling state back; a different game identity for an existing player ID is rejected.

The source freezes a transferring player, processes only matching accepted acknowledgements before removing it, and retries an unacknowledged request after five seconds. Rejected acknowledgements retain the source player. More complete transfer fencing, projectile migration, cross-cell visibility, and service-failure recovery are still Phase 6 work.

**Restart/rebuild gateway and all cell servers together:** the internal player-handoff payload replaces `team` with required domain state. Mixed old/new cell binaries are not supported.

## Combat and future shared rewards

Player collision and projectile hits use the same town-allegiance rule. Each projectile captures `DamageAttribution` at fire time:

- Actual source entity/type (player now; turret, missile, orb, or environment later).
- Beneficiary player ID, game ID, and firing town, independent of source entity ID.

This prevents a turret from needing a player UUID as its own ID, and prevents shooter disconnect/handoff from changing a projectile's team or credit.

Damage contributions retain attribution, damage amounts, and hit times, including across handoff. A lethal hit emits one `DefeatRecord` with a unique event ID, target type/game, finishing credit, contributions and time. These events are now included in state snapshots alongside Hit/Death/Respawn. A future reward service can consume the authoritative record to compute primary, teammate, and assist grants; browser events must never authorize a payout. A durable ledger keyed by defeat event/recipient can prevent duplicate grants across cells. No cash awards or assist eligibility rules are activated here.

`RewardPolicy` leaves team share and assist window unset. `CashShare` validates 0–10000 basis points and computes percentages with integer arithmetic. It represents a configurable percentage of the base award, not a pooled town treasury. Future rules decide who receives it and whether it is additional or redistributive.

## Client contract

Existing player tuples remain compatible. Snapshots additionally carry:

- `catalog_version`
- `self_state`: only the receiving player's identity/wallet/inventory/cooldowns
- `player_affiliations`: public player IDs, town IDs, and roles
- `events`: authoritative combat notifications

`src/game/domain.ts` describes/validates the new private state. Other players' wallets and inventories are not broadcast. Construction, production, pickup/deployment, economy ticking, inventory UI, and new artwork come in later phases.

## Verification

```sh
cargo check --workspace
cargo test --workspace
```

The normal suite covers catalog integrity, distinct rocket/missile recipes, allegiance, wallet limits, reward percentage arithmetic, full nonempty state serialization/adoption/attachment, duplicate/rejected handoffs, death inventory loss, shooter-independent credit, and contribution records.

For an isolated NATS instance, the live two-cell simulation test performs a real handoff followed by WebSocket attachment, asserting nonzero cash, populated inventory, membership, and cooldown preservation:

```powershell
$env:TEST_NATS_URL = 'nats://127.0.0.1:14236'
cargo test -p twz-cellserver live_handoff_preserves -- --ignored --nocapture
```

In the web client, `npm test` checks the extended snapshot boundary and `npm run build` checks strict TypeScript and production bundling.
