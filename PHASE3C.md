# Phase 3c — React construction UI

The client now exposes Phase 3b's authoritative economy and construction commands. [ROADMAP.md](ROADMAP.md) records the gameplay rules; [PHASE3B.md](PHASE3B.md) describes their server implementation. Phase 3d remains the broader integration/failure-window acceptance task.

## Player flow

- **Personal cash HUD:** Shows the current authoritative balance. Before the first economy message it can use the incoming private player snapshot. Once economic state arrives, older movement snapshots and historical command-result balances cannot roll the HUD back.
- **Building catalog:** Loads prices, names, prerequisites and geometry from the gateway's read-only `/catalog` endpoint. Only the intersection of the server's current construction capabilities and the implemented Wall branch is offered. Missing research, insufficient cash, death, being outside one's territory, unavailable metadata and catalog failures have explicit explanations. Catalog failures can be retried; a version mismatch disables construction.
- **Placement:** Select Wall Research or Wall Factory, move the pointer over the world, and click to fix a top-left grid anchor. Choose **Confirm placement** or press **Enter** with the canvas focused. **Move anchor** resumes following the pointer. **Cancel placement**, **Escape**, or right-click exits without sending a purchase.
- **Preview:** Shows the complete reserved footprint and its solid portion separately. Green means locally clear, red gives an invalid-placement explanation, and amber means some terrain is not currently visible and the server must verify it. The server always makes the final decision. Confirming sends only an intention; cash, buildings and refunds are never applied optimistically.
- **Territory:** Tool modes show the gold territory border and grid. A compact territory map, tile bounds and eligibility messages explain town limits without exposing routing cells as placement rules.
- **Inspection/demolition:** Choose **Inspect & demolish**, then click a visible building or select purchased infrastructure from the town-wide list. The selection is highlighted. The inspector shows the original price paid and the server-specified refund percentage, rounded down with integer arithmetic. Confirm demolition or keep the building. Townhalls and other towns' buildings cannot be demolished through these controls. Research removal explains that existing factories remain valid.
- **Controls:** W/S and A/D still move/turn while the canvas is focused. Mouse and Space firing are suppressed in placement/inspection modes, and held inputs are cleared on tool transitions and focus loss. Firing requires a fresh input when returning to play. Tool clicks never implicitly purchase or shoot.
- **Layout:** Desktop uses a viewport and scrollable construction sidebar. Narrow screens stack the viewport and controls without horizontal overflow, including balances at the wallet limit. Keyboard/mouse remain the primary gameplay controls; canvas selection also accepts pointer/touch events.

Factory production, pickups, inventory UI and deployment remain Phase 4. The Wall Factory card explicitly identifies its current infrastructure-only state.

## Reliable command lifecycle

`ConstructionCommands` permits one unresolved action per participation. It creates a UUID, copies the payload, and retains that exact request until its outcome is known.

- A confirmed success or recorded rejection settles the action. A new attempt after a rejection gets a new UUID.
- A timeout, interrupted connection, or unconfirmed service error retains the original UUID/payload and offers **Retry saved request**. It does not create a second purchase or refund intention.
- The pending intention is stored in this tab's `sessionStorage` under `townwarz.pending-construction.v1`, allowing safe retry after reload/resume of the same participation. It contains no Auth0 token. Browser storage failures do not prevent live requests.
- A new participation or a confirmed terminal/quit flow clears the old intention. Unrelated/late result IDs are ignored. Reconnect does not automatically resubmit an uncertain action.
- Result cash is historical; only current economy/snapshot state drives the HUD. No client result handler credits a refund or subtracts a purchase locally.

## Gateway protocol additions

The public message contracts are centralized in `twz-shared/src/economy.rs` as `ConstructionClientMessage`.

`economy_state` now includes:

```text
cash, revision, territory
owned_buildings[]: building instance + original price_paid
constructible_types[], demolition_refund_percent
```

The town-wide owned-building list is necessary to check research and display original-price refunds even when those buildings are outside the current spatial-interest window. Only the connected player's economic state is sent to that browser.

`construction_result` includes `outcome_known` alongside `result`. A durable outcome (including a recorded failure) is final for that request ID. A service/transport failure with an uncertain commit status sets it to false, telling the client to retain the request for replay. Gateway queue/parse rejections are known not to have executed. The result's original request ID remains available for correlation.

`GET /catalog` returns an `Access-Control-Allow-Origin: *` header because its public, read-only definitions are fetched by the SPA on a different development origin. This applies to the catalog response, not authenticated gameplay operations. The client derives the HTTP(S) catalog URL from the WS(S) gateway URL, retaining a reverse-proxy prefix and removing query parameters. Fetches omit credentials and have an eight-second timeout with retry.

Update/restart the **gateway** and refresh/deploy the client together. No new database migration or Auth0 configuration is required. Older gateways without the required economic provenance/known-outcome fields cannot enable the new controls.

## Files

- `twz-webclient/src/game/construction.ts`: Runtime message/catalog validation, eligibility explanations, preview geometry, screen/grid conversion and refund arithmetic.
- `constructionCommands.ts`: Request identity, timeout/replay, persistence and participation scoping.
- `useBuildingCatalog.ts`: Abortable, version-checked catalog loading/retry.
- `useGameConnection.ts`: Authoritative economy messages and command transport integration.
- `ConstructionWorkspace.tsx`: Catalog, preview controls, territory map, inspection and demolition.
- `GameCanvas.tsx`: Grid/territory/ghost rendering, building selection and tool-aware input.

## Verification

From the repository root:

```sh
npm --prefix twz-webclient test
npm --prefix twz-webclient run build
```

The **15 unit/regression tests** cover existing snapshot/entry contracts plus catalog/economy validation, town-wide research, capability/version locks, footprint/terrain/player checks, negative grid coordinates, refund precision, request replay, reload scoping and timeout behavior.

For the browser suite, run from `twz-webclient/`:

```sh
npx playwright install chromium
npm run test:browser
```

The **five Playwright tests** start a dedicated Vite instance on port **5176**, mock Auth0/catalog/WebSocket responses, and exercise the actual React UI:

1. Catalog failure/retry, income display, affordability/research locks, death and territory restrictions.
2. Footprint/territory previews, cancel, movement, and firing suppression across tool transitions.
3. Research/factory purchases and original-price demolition while retaining the existing factory.
4. Lost-confirmation recovery across reload, exact request replay, and protection against historical-balance rollback.
5. Server rejection feedback, protected townhalls and narrow-screen layout at the wallet limit.

These tests do not require real Auth0 credentials or running game services. The root GitHub Actions client job installs Chromium, runs them, and uploads failure traces/screenshots. The real PostgreSQL/NATS/two-cell suite also verifies the gateway's provenance and known-outcome fields. `test-results/` and Playwright reports are ignored by Git.
