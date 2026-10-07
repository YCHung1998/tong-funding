## Why

Two defects found while trading on the demo accounts (evidence: `~/Library/Application Support/tong-funding/funding.db`, pairs `ui-1791301668121-*`):

1. **Leverage differs between the two legs.** The pair snapshot says `leverage: "5"`, but the positions page shows Binance 20× and Bybit 10×. Leverage is only a scan-time number (contract template → `entry_json.scan.leverage` → Node 0 margin estimate); no code ever sends it to an exchange, so each leg uses whatever the account/symbol currently has. Margin, liquidation distance and risk checks are therefore wrong and the legs are not symmetric.
2. **A symbol cannot be re-added to the trade list after it was closed.** The scanner's `staged()` rule treats `CLOSING`, `PARTIAL_FAILURE`, `IMBALANCED` and `UNRESOLVED` as "still staged". In the data: NMRUSDT had `CLOSE_CONFIRMED verified_flat:true` and then sat in `CLOSING` waiting for funding PnL (`PNL_PENDING`, settlement not reached yet); OGNUSDT's legs were flattened by manual orders, so the scheduled close found position 0 and left it in `PARTIAL_FAILURE`. Both are flat, yet the scanner shows "已在交易單" and refuses the tick.

## What Changes

- Before every **opening** order of a pair, the demo executor sets the exchange-side leverage of that symbol to the pair's entry leverage (Binance `POST /fapi/v1/leverage`, Bybit `POST /v5/position/set-leverage`). If setting fails (other than "already that value"), the order is **not sent** (certain rejection, like the position-mode guard). Reduce-only/close orders and manual orders are untouched.
- `OrderRequest` gains `leverage: Option<Decimal>`; the engine fills it from the pair's entry snapshot for opens.
- Scanner eligibility (`staged`) no longer counts a pair as staged once it has no exposure left: `CLOSING` after the flat confirmation (PnL still pending), and `PARTIAL_FAILURE` / `IMBALANCED` / `UNRESOLVED` when both legs' accounts show no position and no open order on the symbol. The pair itself keeps its state, alerts and PnL wait.
- `PairView` exposes `flat_confirmed` (set from the engine's `pnl_wait_since`) so the UI does not guess.

## Capabilities

### New Capabilities
- `order-leverage-sync`: opening orders carry the pair's leverage and the executor aligns both exchanges to it before sending.
- `candidate-readd-after-close`: scanner eligibility ignores pairs that are verifiably flat.

### Modified Capabilities

(none — the staged-order and execution specs live in unarchived changes, not in `openspec/specs/`)

## Impact

- `app/src/engine/ports.rs`, `engine/actor.rs`, `engine/command.rs` (`OrderRequest.leverage`, `PairView.flat_confirmed`) and every `OrderRequest { .. }` construction (tests included).
- `app/src/exchange/execution/{executor,binance,bybit,endpoints}.rs` (leverage requests, executor guard).
- `app/src/ui/vm/candidates.rs` (+ tests).
- Existing positions keep the leverage they have until the next open order of that symbol.
