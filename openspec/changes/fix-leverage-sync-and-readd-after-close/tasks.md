## 1. Leverage on the order request

- [x] 1.1 Add `leverage: Option<Decimal>` to `OrderRequest` (`engine/ports.rs`); fix every literal (`None`), including tests
- [x] 1.2 Engine opening legs (`actor.rs` entry send) set `leverage` from the entry snapshot; close and manual orders stay `None`; test: opening requests carry 5 on both legs, closes carry none (fail-then-pass)

## 2. Executor leverage alignment

- [x] 2.1 `endpoints.rs`: leverage paths; `binance.rs` / `bybit.rs`: `leverage_request` builders + `set_leverage` returning `Result<(), String>` (Bybit 110043 = ok)
- [x] 2.2 `executor.rs`: before an opening submit with `Some(n)`, validate integer 1..=125 then set leverage; any failure ⇒ `not_sent`, order not sent
- [x] 2.3 Tests (`executor_tests.rs` / `contract_tests.rs`): leverage request precedes order; "not modified" proceeds; refusal / rate limit / unknown / 2.5 ⇒ no order; reduce-only and `None` send no leverage request; request shape per exchange
- [x] 2.4 `static_checks` keeps passing (execution dir is exempt from the order-keyword rule); note in `live_probe` or docs that the endpoints are UNVERIFIED on demo

## 3. Re-adding a flat symbol in the scanner

- [x] 3.1 `PairView.flat_confirmed` (`command.rs`), filled from `pnl_wait_since.is_some()` in the actor's snapshot; update constructors
- [x] 3.2 `candidates.rs::staged()`: exempt `CLOSING` + `flat_confirmed`, and locked states when both legs' `leg_accounts` lists are `Ok`/complete and empty for the symbol; unknown ⇒ staged
- [x] 3.3 Tests in `candidates_tests.rs` for every spec scenario (closing confirmed, closing unconfirmed, locked flat, locked with position, unknown read, old pair unchanged); reproduce the NMR/OGN cases from the recorded data
- [x] 3.4 Pin test (pnl): a second pair on the same symbol does not alter the first pair's PnL assembly
- [x] 3.5 Engine: flat-confirmed `CLOSING` pairs skip the `max_concurrent_pairs` count; test with `max_concurrent_pairs = 1` (fail-then-pass)

## 4. Verify

- [x] 4.1 `cargo test -p tong-funding -p tong-funding-core` green; `openspec validate fix-leverage-sync-and-readd-after-close` passes
