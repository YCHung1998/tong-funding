## Why

The contract-settings leverage is the value every staged order must trade at (`fix-leverage-sync-and-readd-after-close` sends it to both exchanges before the opening order). Exchanges cap leverage per symbol, and the cap shrinks when the symbol is high-risk or the position is large (Binance leverage brackets, Bybit symbol `maxLeverage`). Today nothing asks the exchange for that cap: a leverage above it is only discovered when the set-leverage request is refused, after one leg may already have been sent. The user must see the cap when the order is staged and have the system verify that the chosen leverage complies.

## What Changes

- New read-only account queries: per-symbol maximum leverage for a position notional — Binance `GET /fapi/v1/leverageBracket` (the bracket that contains the notional), Bybit `GET /v5/market/instruments-info` (`leverageFilter.maxLeverage`).
- Staged-orders page: each PREPARED pair shows the cap of both legs and whether its leverage complies; a pair whose leverage is above a cap is not selectable for one-click submit (reason names the exchange and the cap). In EXCHANGE_DEMO an unknown cap is also not selectable; in SIMULATION it is informational only.
- Engine pre-trade: for EXCHANGE_DEMO pairs both caps are fetched fresh and the pair is BLOCKED when the leverage exceeds either cap or a cap cannot be read (fail closed), before any order or leverage request.
- Candidate list (scanner): shows the same cap check and refuses "add to staged orders" for a symbol whose known cap is below the contract leverage.
- A live probe (ignored, env-gated, like the existing demo probe) exercises the leverage-cap read and the leverage-set requests against the real demo hosts.

## Capabilities

### New Capabilities
- `symbol-leverage-cap`: per-symbol leverage cap lookup, display, and verification (UI + engine).

### Modified Capabilities

(none)

## Impact

- `app/src/engine/ports.rs` (`AccountView::max_leverage`, default `Err`), `engine/actor.rs` (entry context + Node 0 gate), `exchange/signed/{binance,bybit,endpoints}.rs`, `exchange/execution/account.rs`.
- `app/src/ui/vm/{bridge,staged_orders,candidates}.rs`, `ui/live.rs`, `ui/trading_pages.rs` (cap loader, display).
- Tests: parsers on recorded-shape fixtures (Bybit fixture is a real demo response), view-model and engine tests, ignored live probe.
