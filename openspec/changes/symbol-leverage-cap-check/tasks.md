## 1. Cap lookup (read-only API)

- [x] 1.1 Pure parsers + tests: Binance `leverageBracket` by notional, Bybit `instruments-info` `maxLeverage` (Bybit fixture = real demo response) — `signed/leverage_cap_tests.rs`
- [x] 1.2 Signed clients: `get_max_leverage` on Binance (signed GET with parameters) and Bybit; endpoint constants
- [x] 1.3 `AccountView::max_leverage` (default `Err`), `DemoAccountView` implementation; tests through the fake transport (`executor_tests.rs`: request shape, per-notional bracket, failures are errors)

## 2. Engine gate

- [x] 2.1 Fetch both caps in the entry-context step for EXCHANGE_DEMO pairs; blocked with `LeverageCap` (above cap / unreadable); tests in `flow_tests.rs` (fail-then-pass shown by disabling the gate)

## 3. UI

- [x] 3.1 `UiSnapshot.leverage_caps`, `SourceUpdate::LeverageCap`, pure `leverage_cap` view-model rule + tests
- [x] 3.2 Staged-orders row: caps shown, selectability rule, reasons; tests
- [x] 3.3 Candidate list: cap shown, add refused when a known cap is below leverage; tests
- [x] 3.4 Cap loader in `live.rs` (requests de-duplicated to one per 20 s per key) fed by the pages' renders

## 4. Real API verification

- [x] 4.1 Ignored, env-gated live probe `live_leverage_probe` (cap reads, leverage set through the real executor, read-back of both positions' leverage, refusal above the cap)
- [x] 4.2 Run on the Mac against the real demo accounts (2026-10-07, ADAUSDT qty 30, leverage 5): caps read Binance 75 / Bybit 75; both opening orders accepted; read back Binance position `leverage: 5`, Bybit position `leverage: 5`; both closed and flat; leverage 76 (cap+1) refused by Binance `-4028: Leverage 76 is not valid` → `not_sent`, no position left
- [x] 4.3 Full `cargo test -p tong-funding -p tong-funding-core` green (1298 passed, 5 ignored); `openspec validate` for both changes
