## 1. Storage

- [x] 1.1 `schema_v3.sql` + migration 3 + `LATEST_SCHEMA_VERSION = 3`; `Db` slot API (insert-if-absent, due query, update); `PAIR_FUNDING_LINKED` event writer (one per linked row, idempotent); migration and uniqueness tests

## 2. State machine (pure)

- [x] 2.1 `funding/slots.rs`: `step(row, now, check)` with the constants 5/10/30/60 min; table-driven tests for every transition incl. failed checks, skipped time, reasons `NOT_FOUND`/`FETCH_FAILED`, and `ZERO_RATE`

## 3. Creation and checking

- [x] 3.1 Slot creation from the entry snapshot grid and our held-position records (past `t` or at close) for Binance/Bybit/OKX legs; none for simulated or closed-before-`t`
- [x] 3.2 Due-slot checks merged per `(exchange, symbol)` through the existing ledger sources and limiter; match with the existing tolerance; link or record ambiguity; catch-up on start and after a loop gap (one immediate check per overdue slot)
- [x] 3.3 Public funding-rate history client (Binance, Bybit, OKX) + the `ZERO_RATE` resolution; parser tests on recorded-shape fixtures
- [x] 3.4 `MANUAL_CHECK` re-check command and hourly sweep (3 days)

## 4. PnL and backfill

- [x] 4.1 `pnl_record` reads slots (COMPLETE only when all `SETTLED`, reasons per tracing state) and recomputes on each `SETTLED`
- [x] 4.2 One-time backfill of finalized INCOMPLETE pairs; delay percentile helper

## 5. UI

- [x] 5.1 Positions page: tracing state per pair/leg + "重新查詢" button

## 6. Verification

- [x] 6.1 Full `cargo test -p tong-funding -p tong-funding-core`; `openspec validate funding-slot-tracing`
- [ ] 6.2 Real demo check (user present; `live_funding_slot_probe` repairs-on-a-copy the earlier INCOMPLETE pairs against the real ledgers and prints every slot, plus a tiny position held through one settlement for the delay and the zero-rate question): hold a tiny position through one settlement, record the first-seen delay, confirm the slot settles and the PnL completes; read the public funding-rate history of all three exchanges and see whether a zero-rate settlement produces a ledger row
