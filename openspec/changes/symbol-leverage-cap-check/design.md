## Context

- Leverage source of truth: contract template (`config.contract_template.leverage`) → candidate `leverage` → `pairs.entry_json.scan.leverage` → `OrderRequest.leverage` → set on both exchanges before the opening order.
- Exchange caps: Binance `leverageBracket` lists brackets `{initialLeverage, notionalFloor, notionalCap}`; the allowed leverage for a position depends on its notional (a bigger position gets a smaller cap). Bybit `instruments-info.leverageFilter.maxLeverage` is the symbol cap (live demo example: NMRUSDT "50.00").
- `AccountView` is the engine's read-only signed port; `DemoAccountView` wraps the signed GET clients. The UI has a loader pattern for lot rules (`rules_tx`) and a store refresher that already iterates the pairs and their entry snapshots.

## Goals / Non-Goals

**Goals:** show the cap where the order is staged; verify compliance in the UI and again (fail closed) in the engine before anything is sent; real-API verification.

**Non-Goals:** automatically lowering the leverage; Bybit risk-limit tiers (the set-leverage refusal remains the last backstop); changing the contract-settings page's own warning (it keeps comparing with the configured `max_leverage` only); OKX (never traded).

## Decisions

1. **Port:** `AccountView::max_leverage(exchange, symbol, notional) -> Result<Decimal, String>` with a default `Err("unsupported")`, so fakes and the simulated ledger need no change. Alternative (a separate `LeverageLimits` trait) rejected: it would need the same wiring for the same signed clients.
2. **Binance bracket choice:** the bracket with `notionalFloor <= notional <= notionalCap`; a notional beyond the last bracket is an error (no cap can be named). `symbol` is passed so only that symbol is returned. Needs a signed GET with extra query parameters: `attempt`/`signed_get` take a parameter list (existing callers pass none).
3. **Bybit:** `maxLeverage` of the symbol from `instruments-info` (category linear), read through the signed client's GET (public endpoint, signing is harmless and keeps one request path). A symbol that is not `Trading`, or a missing field, is an error.
4. **Freshness / loader:** the store refresher already ticks with the pair rows; for every PREPARED pair it sends `(exchange, symbol, notional)` to a cap loader task (spawned where the demo `account` view exists). The loader skips a key fetched less than 20 s ago and pushes `SourceUpdate::LeverageCap` into `UiSnapshot.leverage_caps[(exchange, symbol)] = CapReading { notional, cap: Result, fetched_at }`. The scanner candidate list requests the same for its ticked rows. A reading older than 60 s, or taken for a different notional, counts as unknown.
5. **UI rule (pure view-model):** `cap_check(leverage, readings, mode)` → `Ok(caps)` / `Exceeds{exchange, cap}` / `Unknown{why}`. Staged row: `Exceeds` ⇒ not selectable ("槓桿 5× 超過 Bybit 上限 3×"); `Unknown` ⇒ not selectable in EXCHANGE_DEMO, informational in SIMULATION. Candidate list: `Exceeds` blocks adding; `Unknown` only shows "上限未知".
6. **Engine gate:** in the entry-context fetch (EXCHANGE_DEMO pairs only) both caps are read fresh for the pair's notional; Node 0's verdict is overridden by a block `LeverageCap` when leverage > cap or a read failed. The block lands in the same `CheckFailed` event as other blocks (`failed_checks` contains `LeverageCap`). SIMULATION pairs skip it (no exchange involved).
7. **Two layers stay:** the executor's set-leverage refusal (previous change) is the final backstop; the new gate makes a refusal an exception instead of the discovery mechanism, and it happens before either leg is sent.

## Risks / Trade-offs

- [Binance leverageBracket / Bybit field shapes on the demo hosts] → a live, env-gated probe calls both and prints the results; parsers are tested with recorded-shape fixtures (Bybit fixture captured from the real demo host).
- [Cap changes between the pre-trade read and the order] → set-leverage refusal still prevents the order (design 7).
- [Extra signed requests every refresher tick for PREPARED pairs] → throttled to one per key per 20 s; only PREPARED pairs and ticked candidates.
- [A stale or missing reading blocks selection in EXCHANGE_DEMO] → intended fail-closed; the page says why and refreshes on the next tick.
