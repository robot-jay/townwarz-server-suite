# TownWarz Roadmap

## Current Progress

| Milestone | Status |
|-----------|--------|
| Phase 1 — Domain and authority foundations | Implemented; see [PHASE1.md](PHASE1.md) |
| Phase 2 — Physical buildings and reconnect lifecycle | Implemented; see [PHASE2.md](PHASE2.md) and [EXPIRY_ENTRY.md](EXPIRY_ENTRY.md) |
| Phase 3a — Construction decisions | Complete; agreed rules below |
| Phase 3b — Server economy and construction | Implemented; see [PHASE3B.md](PHASE3B.md) |
| Phase 3c — React construction UI | Implemented; see [PHASE3C.md](PHASE3C.md) |
| Phase 3d — Integration verification and acceptance | Next task |
| Phases 4–8 | Planned |

Descriptions of the former "current" implementation in the original Phase 1–2 sections are historical starting-point context. Their completion documents describe the implemented behavior. Phase 3a below records the subsequent decisions that refine the original plan.

## Phase 1 — Establish the Domain Model and Authority Rules

**Goal:** Give every gameplay object a clear identity, owner, and lifecycle.

### Shared Definitions

Add versioned definitions in `twz-shared` for:

- `BuildingType`
- `ItemType`
- `ItemBuildingType`
- Building costs and prerequisites
- Placement footprint and collision shape
- Health and destructibility
- Production recipes and durations
- Pickup location
- Item-use behavior
- Targeting, damage, range, and cooldowns

Separate **definitions** from **instances**. A Wall Factory definition describes its cost and recipe; a particular Wall Factory instance stores its ID, town, position, health, production deadline, and completed output.

Use stable named identifiers rather than relying on matching numeric values between unrelated enums.

### Player and Town State

Extend authoritative player state with:

- Town ID
- Role
- Cash
- Six inventory slots
- Relevant cooldowns
- Current game/session ID

The current cell-server `Player` has none of that economic state. Its allegiance is still a UUID-derived two-team value.

**Replace that allegiance model with town-based ownership before implementing defenses.** Otherwise unrelated towns can randomly be treated as allies.

Projectiles also need to retain their firing town and reward attribution independently of whether their owner is still connected.

### Authority Boundaries

| Component | Responsibility |
|-----------|---------------|
| Gateway | Authenticate and attach commands to the connected player |
| Conductor/Town Authority | Game lifecycle and town-wide coordination |
| Cell Server | Authoritative simulation and spatial entity ownership |
| PostgreSQL | Durable records, checkpoints, and history as required |
| React Client | Display state and submit player intentions |

Keep PostgreSQL out of the per-frame simulation loop.

**Completion Criterion:** Player role, town, cash, and inventory can be represented and transferred without losing state.

---

## Phase 2 — Implement Real Building Entities and Placement Geometry

**Goal:** Buildings become authoritative world objects rather than just visuals.

The current townhall is a terrain tile plus town metadata. It needs an actual building identity with a footprint, collision rules, health policy, and interaction locations.

### Necessary Changes

- Add building collections to cell-server world state.
- Establish consistent coordinate conventions:
  - Tile coordinates for placement.
  - World coordinates for movement.
  - Explicit top-left anchors versus player-center coordinates.
- Separate:
  - Placement footprint.
  - Physical collision shape.
  - Rendered dimensions.
  - Pickup and interaction points.
- Add dynamic building collision to player movement and projectiles.
- Maintain a spatial occupancy index for placement checks.
- Ensure terrain generation provides sufficient usable space for the chosen building footprints.

**Recommendation:** Restore the legacy 3×3 placement footprint and front access area rather than enlarging the current townhall sprite without changing collision.

### Lifecycle Requirement

Every entity must be associated with its town/game lifecycle. When a game ends, its buildings, output items, and deployments must be removed before the hall is reused.

The current active-town refresh only removes town metadata; it would not automatically remove newly introduced structures.

**Completion Criterion:** A building can be created, rendered, collided with, and removed consistently.

---

## Phase 3 — Add Cash, Research Prerequisites, and Construction UI

**Goal:** A mayor can legitimately purchase and place infrastructure.

### Phase 3a — Clarify and Record the Construction Rules

**Status:** Complete. The following decisions are the implementation contract for Phases 3b–3d.

The Phase 3 construction catalog, Mayor income timing, spatial authority restrictions, purchase recovery guarantees, placement rules, and demolition/prerequisite behavior are agreed below so the server, client, and acceptance tests share one specification.

Already established by this roadmap and the implemented foundations:

- Cash belongs to the individual player; a new participation starts at zero.
- Use legacy income and catalog costs as initial tuning, with server-controlled development settings for faster testing. The legacy Mayor baseline is 100 cash per three seconds.
- Only a living Mayor with an active game may construct infrastructure.
- Server definitions determine price, geometry, prerequisites, and ownership; client previews are advisory.
- Research buildings gate factory construction by their current presence, rather than permanently unlocking research.
- Request IDs and authoritative spending/creation must prevent duplicate purchases.
- The minimum Phase 3 milestone is Wall Research followed by Wall Factory.
- Manufacturing, pickups, inventory UI, and Wall deployment remain Phase 4 work.
- Actual Commando membership, succession, and surviving-town rejoining are separate work; their future contracts do not enable those features in Phase 3.

#### Confirmed Phase 3a decisions

- **Catalog:** Only Wall Research and Wall Factory become purchasable in Phase 3. Factory production and deployment remain Phase 4.
- **Spatial authority:** A town's buildings belong to the authority for its townhall's routing cell. Every town has finite buildable territory, and hall placement/territory geometry must keep the entire territory within that cell. Players should encounter town-territory rules, not arbitrary cell-boundary restrictions. Phase 3 remains bounded; do not enable unrestricted construction across cells.
- **Income:** Use 100 cash per three seconds as the normal Mayor baseline. Income accrues while connected, including death/respawn, pauses on disconnect, and preserves partial eligible-time progress through reconnect/handoff. Disconnected time grants no catch-up payment, and reconnect/handoff grants no extra payment. Development tuning is server-controlled.
- **Recovery:** Purchases must be durable. Persist/recover request outcomes, spending, and building creation consistently so acknowledged purchases cannot become free buildings or lost purchases after service failure. A stale player checkpoint must not overwrite a committed economic result. This brings the necessary economic/building recovery work forward; movement remains simulation/checkpoint based rather than per-frame database writes.
- **Territories:** Keep current hall generation and define one nonoverlapping 32×32-tile (1,536×1,536-pixel) territory per hall, aligned to the existing 32-tile hall-site grid. A 64×64-tile routing cell contains four such territories. For each axis, the territory's minimum tile is `floor(hall_origin_tile / 32) * 32`, and its exclusive maximum is that minimum plus 32. Use mathematical floor/Euclidean division for negative coordinates. The entire reserved building footprint, including any forecourt, must fit inside the territory. Touching its edge without extending outside is valid.
- **Range:** Territory containment is the effective build-range rule for this milestone, rather than granting a 1,500-pixel circle across territory edges. The old catalog distance must not be the only placement check. This choice preserves existing hall locations and requires no hall relocation solely to establish territories.
- **Mayor proximity:** A living, connected Mayor must be physically inside their own town's territory to construct or voluntarily demolish infrastructure there. There is no additional avatar-to-building interaction-distance limit. Validate the Mayor's authoritative position and route to the townhall-cell authority; the browser cannot choose the actor, owning town, authority, or territory.
- **Demolition:** Only owned, non-townhall buildings may be voluntarily demolished. Refund **25% of the amount actually paid for that building**, rounded down to whole cash units. Persist that purchase price as economic provenance; never recompute a refund from a later catalog price. At normal prices, Wall Research refunds 1,250 and Wall Factory refunds 12,500. Remove the building and apply its refund as one recoverable, idempotent operation. Game-end cleanup is not voluntary demolition and does not award this refund.
- **Research removal:** Research gates new factory purchases only. Voluntarily demolishing Wall Research leaves existing Wall Factories valid; it does not cascade demolition or turn their future production off. Another Wall Factory purchase requires Wall Research to be present again. Phase 4 must preserve this rule.
- **Player obstruction:** Reject construction if the new solid collider intersects any living player's collision circle, including the builder. A player occupying only the walkable forecourt does not invalidate placement. Check at authoritative commit time, not merely during the client's preview. Full anti-enclosure/pathfinding guarantees are outside this milestone.

Territory containment solves spatial ownership but does not itself replicate neighboring buildings for visibility/collision. Neighboring-cell projections and seamless boundary interaction remain explicit Phase 6 work; do not claim that territory geometry alone completes distributed-world correctness.

#### Phase 3b implementation obligations

- Expose authoritative territory geometry and construction eligibility so the UI can display town limits without referring to routing cells. Keep stable cell assignments during this bounded milestone; reassignment requires recovery/coordination rather than silently creating a second authority.
- Serialize income, purchases, and demolition against the authoritative player's wallet. Preserve eligible-time progress through handoff/reconnect without duplicate income or disconnected-time catch-up. New games remain at zero until their first completed income interval.
- Define shared command/result contracts in `twz-shared`, including structured rejection reasons and request IDs scoped to the authenticated participation. The browser submits intentions, not price, ownership, health, refunds, or complete building instances.
- Validate all placement rules and commit the debit/creation together. Failed placement must not spend cash or reserve occupancy. Competing requests cannot claim the same footprint.
- Persist successful purchase/demolition outcomes, building economic provenance, and the matching wallet changes. Duplicate requests return their recorded outcome without repeating mutations; reusing an ID with a different payload must be rejected.
- Acknowledgement follows the durable commit. Recovery must reconstruct committed buildings/removals and wallet effects even if a service fails between database commit, simulation application, and client acknowledgement. Resume from an older checkpoint must not restore already-spent/refunded cash over the committed economic state.
- Reconcile durable infrastructure with the active-town lifecycle. Ended games must not regain buildings after restart or delayed replay, and an old game's cleanup must not affect a new town reusing the hall.
- Keep database work outside the per-frame simulation loop. Use a recoverable command/persistence boundary and explicit simulation application; a memory-only mutation plus an unrelated checkpoint is insufficient.
- Add configurable server-side development income tuning with production defaults of 100 cash per three seconds. Clients cannot select faster income or bypass construction restrictions.

No further gameplay decisions are outstanding for Phase 3b. Database schema and internal command/application mechanics are implementation work under these guarantees; any newly discovered gameplay ambiguity should be clarified rather than silently assigned a rule.

**Completion criterion:** These decisions are agreed and recorded, and Phase 3b has an unambiguous server-side specification.

### Phase 3b — Implement Server Economy and Construction

**Status:** Implemented. See [PHASE3B.md](PHASE3B.md) for the economic transaction/recovery boundary, shared protocol, configuration and server verification. UI-level completion remains Phase 3c–3d.

Add authoritative Mayor income using the Phase 3a eligibility/timing rules, including state needed to preserve timing through handoff and reconnect. Apply the agreed development tuning, authority boundary, and recovery policy.

#### Server Commands

Introduce commands such as:

- `place_building`
- `demolish_building`

Commands should include a request ID and the requested building type/location. The server derives the acting player from the connection and derives cost, dimensions, and ownership from authoritative definitions.

Validate:

- Active game and living player
- Mayor role
- Valid building type
- Sufficient cash
- Research prerequisite
- Placement distance
- Full town-territory containment and Mayor presence within their own territory
- Terrain and footprint availability
- No new solid collider overlapping a living player
- Ownership for demolition
- Townhall demolition restrictions
- Recorded-price, 25%-rounded-down demolition refund and replay protection

**Cash deduction and successful creation must be one authoritative operation.** Repeated commands must not spend twice or create duplicates.

**Completion criterion:** Automated server tests prove income, valid and rejected purchases, prerequisite enforcement, demolition, and request replay behavior under the Phase 3a rules.

### Phase 3c — Implement the React Construction UI

**Status:** Implemented. See [PHASE3C.md](PHASE3C.md) for controls, protocol metadata, safe command replay, browser coverage and deployment. Phase 3d remains the broader integration/failure-window acceptance task.

Add:

- Cash HUD
- Building catalog with prices
- Locked/unaffordable states with explanations
- Grid-snapped placement preview
- Valid/invalid footprint feedback
- Visible town-territory boundaries and explanations for being outside one's territory
- Confirm/cancel controls
- Building selection and demolition controls
- Clear command rejection messages

**Completion criterion:** A player can inspect server-defined prices/restrictions, preview placement, submit construction or demolition, and see authoritative results and errors.

### Phase 3d — Integration Verification and Acceptance

- Exercise the full income → Wall Research purchase → Wall Factory purchase flow through the authenticated gateway and real cell simulation.
- Verify failed/replayed requests cannot create extra buildings or spend twice, and competing placements cannot overlap.
- Verify repeated demolition cannot refund twice, and refunds use the recorded price with integer rounding.
- Verify active/respawning income, partial-interval preservation, and no disconnected-time catch-up or reconnect bonus.
- Verify demolition, prerequisite changes, and game-end cleanup follow the Phase 3a decisions.
- Verify reconnect/handoff, boundary restrictions, and service-recovery behavior against the explicitly agreed guarantees.
- Exercise failure windows around durable commit, simulation application, and acknowledgement; verify old checkpoints and request replays cannot undo economic results or resurrect removed/ended-game assets.
- Verify the construction UI's preview/confirm/cancel/error behavior, including the interaction with movement and firing.
- Include the new automated coverage in the root monorepo CI workflow and document any intentional early-milestone limitations.

**Phase 3 completion criterion:** A mayor can earn cash, build Wall Research, then build a Wall Factory through the UI, with server-enforced spending and prerequisites and the agreed lifecycle/recovery guarantees. Factory production and Wall deployment begin in Phase 4.

---

## Phase 4 — Complete the First Production/Inventory/Deployment Loop

**Goal:** Deliver Walls as the first complete playable branch.

### Factory Production

Implement explicit factory states:

`Producing → Output Ready → Output Collected → Producing`

Use simulation deadlines for production and expose progress to clients.

A full factory stops producing additional items until its output is collected.

Per Phase 3a, research presence gates factory purchases, not continued operation: an existing factory remains valid and may produce after its research building is voluntarily demolished.

### Inventory and Pickups

Implement six slots initially, matching the legacy model.

Pickup must atomically:

1. Verify the item still exists.
2. Verify player eligibility and proximity.
3. Find an available slot.
4. Transfer that exact item.
5. Clear the factory's output.

If inventory is full, the item stays available. If two players reach it together, only one receives it.

### Wall Deployment

Consume the selected Wall item only after placement succeeds. Validate its actual inventory type, footprint, placement range, and obstruction.

### React Work

- Factory progress/readiness display
- Pickup rendering
- Six-slot inventory bar
- Slot selection and deployment mode
- Wall placement preview
- Full-inventory feedback

**Completion Criterion:**

Earn cash → Build research → Build factory → Wait → Collect Wall → Deploy Wall → Empty inventory slot.

This is the best first milestone because it proves all three concepts together.

---

## Phase 5 — Add Combat, Destruction, and Turrets

**Goal:** Infrastructure and deployments participate in combat.

### Shared Combat Pipeline

Extend damage handling to:

- Players
- Buildings
- ItemBuildings

Add explicit destruction events, entity removal, output cleanup, and ownership-based reward attribution.

Make destruction idempotent so simultaneous hits cannot award duplicate rewards.

Decide whether to restore the legacy score/cash rewards at this stage; keep those values in definitions.

### Turrets

Implement:

- Enemy selection by town
- Target acquisition range
- Target validity checks
- Rotation/aiming
- Firing cooldown
- Projectiles attributed to the turret and its owner/town
- Destruction and target clearing

### React Work

Add health bars, damage feedback, factory smoke, turret facing/firing, and destruction effects.

The current server generates combat events but omits them from outgoing snapshots. That delivery path needs to be completed for reliable effects and notifications.

**Completion Criterion:** A deployed turret attacks enemy-town players, respects allegiance, and can be destroyed.

---

## Phase 6 — Make the Systems Correct Across Cell Boundaries

**Goal:** Production, construction, inventory, and combat remain correct in the distributed server.

This needs to happen before claiming full parity. Phase 3a replaces unrestricted use of the old 1,500-unit build radius with finite 32×32-tile town territories contained within routing cells. That bounds building authority, but nearby players/projectiles can still interact across a routing-cell edge, and live cell reassignment still requires coordination.

### Necessary Work

- Assign one authoritative owner to every entity.
- Route construction to the relevant town/spatial authority.
- Coordinate individual player wallets with operations handled by another spatial authority when needed; cash is not a pooled town treasury.
- Include buildings whose footprints overlap a neighboring cell.
- Replicate nearby entities across cell boundaries for visibility, collision, and targeting.
- Transfer moving ItemBuildings and projectiles between servers.
- Extend player handoff to include inventory, cash, role, town, and cooldowns.
- Prevent duplicate transfers and repeated spending during retries.
- Propagate game-end cleanup to every cell containing that town's entities.

For the early Wall milestone, Phase 3a's finite town territories bound construction to the townhall's routing cell. Preserve the player-facing finite-territory rule in the full implementation; remove the technical visibility/collision/coordination seams through explicit ownership and cross-cell coordination. Phase 3b already establishes durable construction spending and demolition recovery; extend those guarantees to production, pickups, deployments, combat, and reassignment rather than introducing a separate economic authority.

**Completion Criterion:** Crossing a boundary does not reset inventory/cash, duplicate pickups, or make defenses disappear.

---

## Phase 7 — Implement Missiles and Orb-Driven Town Defeat

### Homing Missiles

Port the intended behavior into elapsed-time-based simulation:

- Launch from the player's position and facing.
- Acquire appropriate enemy targets.
- Accelerate to a defined maximum speed.
- Track direction.
- Respect maximum travel distance.
- Collide, deal damage, and expire.
- Preserve ownership through cell transfers.

Legacy movement is expressed in units per update; those numbers need conversion rather than direct copying.

### Orbs

Implement an explicit lifecycle:

`Valid enemy-hall placement`

↓

`Armed countdown`

↓

`Destroyed/disarmed OR detonated`

↓

`Authoritative town-defeat operation`

Resolve the actual enemy hall from the placement target, enforce proximity, and expose the countdown.

Town defeat must consistently handle:

- Buildings and factory outputs
- Deployed defenses
- Players and projectiles
- Rewards
- Active game records
- Hall release
- Client defeat messaging

**Recommended fit with your current rules:** Defeating a mayor ends that game and returns them to new-game setup. This needs to be a distinct terminal event rather than looking like an unexpected socket failure.

**Completion Criterion:** An Orb can be planted, defended against, or detonated, producing exactly one coherent town-defeat outcome.

---

## Phase 8 — Complete the Remaining Catalog

The legacy source does not fully specify every declared type. This phase needs a behavior specification for each remaining entry.

### Missing Decisions / Implementation

| Content | Missing Decisions / Implementation |
|---------|----------------------------------|
| Hospital | Healing behavior, radius, rate, eligibility |
| House | Actual gameplay benefit: capacity, income, or another effect |
| Laser Research / Factory | What is manufactured and how the player uses it |
| Cloak | Production source, duration, visibility and targeting effects |
| MedKit | Production source, healing amount, use restrictions |
| Time Bomb / Bomb | Placement, fuse, damage, blast radius |
| Mine | Trigger rules, arming, visibility, detonation |
| Plasma Turret / Plasma | Whether Plasma is ammunition, a weapon upgrade, or a deployed turret |
| Bazooka / Rocket | Relationship between ordinary rockets and the existing homing missile |
| Sleeper | Weapon behavior and any status effect |
| Flare | Visibility/reveal behavior, duration, projectile interaction |
| DFG | Intended effect and rules; the server provides no substantive implementation |

Not every item should necessarily become an ItemBuilding. The model should explicitly support **deploy**, **consume**, **equip**, and **fire** behaviors.
