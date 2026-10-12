## Context

- Exists: ledger fetch + store (`funding/fetch.rs`, event `FUNDING_LEDGER_ENTRY` with unique `dedupe_key`), `core::pnl::{expected_settlements, match_slots, attribute}`, `funding/runner.rs` (15 s loop), `pnl_record` (PnL assembled from events, `PAIR_PNL_COMPUTED/RECOMPUTED`), `engine/actor/pnl_gate.rs` (CLOSING waits up to 10 min, then FINALIZED with `INCOMPLETE`).
- Gap found in `funding.db`: pair 1 (NMR, 10-07 00:00) completed because the app stayed up (entries seen between settlement+145 s and +267 s); pair 2 (12:00) lost its window to sleep/offline and stayed `INCOMPLETE`. Ledger events have `pair_id = NULL`.
- Funding grids differ per leg (Binance 8 h, Bybit 1 h for NMRUSDT), so a settlement is tracked per leg.
- Absence of a row is legitimate when no position was held at the settlement instant (OGN: both legs closed by hand before 00:00, Binance returned nothing for 10 minutes). Absence must never be inferred from a failed or missing query.

## Goals / Non-Goals

**Goals:** survive sleep/offline/restart; make "not there yet", "really nothing" and "could not ask" distinguishable; link each fee to its `pair_id` once, durably; make late rows recoverable; collect delay data.

**Non-Goals:** WebSocket funding streams (REST is the reconciliation truth; Binance `ACCOUNT_CONFIG_UPDATE` is not a funding event); the four-order trace record; the history page.

## Decisions

1. **Table `funding_slots` (schema v3):** one row per `(pair_id, leg, settle_ms)`, unique. Columns: `exchange`, `symbol`, `state`, `manual_reason`, `amount`, `ledger_dedupe_key`, `ledger_exchange_id`, `first_seen_ms`, `next_check_ms`, `checks_ok`, `checks_failed`, `last_error`, `created_ms`, `updated_ms`. The working state lives here; the immutable record is the appended `PAIR_FUNDING_LINKED` event (payload: pair, leg, settle time, amount, dedupe key, exchange id, first-seen, delay). Alternative (only events, derive state) rejected: staged scheduling needs cheap "what is due" queries and idempotent updates.
2. **When a slot is created:** for every non-simulated leg on Binance, Bybit or OKX (OKX has its own ledger source since `okx-funding-ledger`), each grid time `t` (from the entry snapshot's `next_funding_time` + interval, `expected_settlements`) with `opened < t ≤ closed_or_now` while our own records say the position was held. Created by the funding loop when `t` has passed ("過結算點") or at close, whichever comes first. A close before `t` creates no slot, so no false "missing". Simulated pairs never create slots.
3. **State machine (pure function `step(row, now, check) -> row'`, elapsed time since `t` decides the stage):**
   - `TRACING_1`: due at `t+5, +10, … +30 min`; found → `SETTLED`; at `t+30` still not found → `TRACING_2`.
   - `TRACING_2`: due at `t+40, +50, +60 min`; found → `SETTLED`; at `t+60` still not found → `MANUAL_CHECK`.
   - `MANUAL_CHECK` reason: `NOT_FOUND` when the last checks completed and found nothing; `FETCH_FAILED` when the last check at or after the deadline failed.
   - A check is `Found` / `NotFound` (query window completed and covers `t`) / `Failed`. `Failed` never advances toward "nothing there" and does not reset the clock; stages follow elapsed time.
   - The settlement leg pairs: a pair-level settlement is complete when all its leg rows are `SETTLED`.
4. **Catch-up:** `next_check_ms` is stored; on start (and after sleep, detected by a gap between ticks) every overdue row is checked immediately once before its stage is decided from elapsed time. A row older than 60 min at that point still gets this one check before becoming `MANUAL_CHECK`.
5. **Matching:** reuse `match_slots` tolerance (±60 s around `t`); queries are shared per `(exchange, symbol)` (Binance per symbol, Bybit all symbols) over `[slot t − tolerance, now]`; an entry already linked to another row is never reused (dedupe key unique in the table).
6. **`MANUAL_CHECK` is not final:** rows stay queryable; an hourly low-frequency sweep for 3 days after `t`, and a user "重新查詢" command, both run the same check; a hit moves the row to `SETTLED`.
7. **PnL:** `pnl_record` reads linked slot rows for pairs that have slots (legacy window attribution stays for pairs without slots). The PnL is `COMPLETE` only when every slot is `SETTLED`; otherwise `INCOMPLETE` with a reason naming the tracing state ("資費追蹤中" / "需人工檢查"). Every `SETTLED` transition triggers `recompute_if_changed`. The pair's own lifecycle is untouched: `pnl_gate` still finalizes after its existing window, the later recompute completes the PnL.
8. **Backfill:** a one-time pass inserts slots for finalized, non-simulated pairs whose PnL is `INCOMPLETE` with missing settlements, in `MANUAL_CHECK` with `next_check_ms = now`, so they are retried by the same sweep (this repairs NMR pair 2).
9. **Calibration data:** every `SETTLED` records `first_seen_ms − t`; a small read helper reports p50/p95/p99 per exchange. Constants 5/10/30/60 min, 3 days and the 60 s tolerance are named constants.

10. **Zero funding rate (user: "資金費率有可能是 0"):** when a check completed without a ledger row, the system asks the exchange's **public funding-rate history** for the rate at `t` (Binance `GET /fapi/v1/fundingRate`, Bybit `GET /v5/market/funding/history`, OKX `GET /api/v5/public/funding-rate-history`). A rate of exactly 0 settles the slot at once as `SETTLED` with `amount = 0` and `resolution = ZERO_RATE` (no ledger id; the rate and its source are stored). A non-zero rate with no ledger row keeps tracing and ends in `MANUAL_CHECK/NOT_FOUND` (an anomaly worth a human look). A failed rate lookup changes nothing (the slot keeps its normal path). If the exchange also emits a 0-amount ledger row, the row wins and the slot settles normally with `resolution = LEDGER`.

## Risks / Trade-offs

- [Funding rate exactly 0 may produce no ledger row] → decision 10 settles it from the public rate history; the live check (task 6.2) shows whether the demo hosts emit a zero-amount row at all.
- [Two pairs on one symbol/exchange overlap in a slot] → the existing ambiguity rule stays; an ambiguous match is not linked and the slot keeps tracing (reason recorded).
- [Clock skew between local time and exchange time] → stages use the local clock against `t`; the tolerance and 5-minute first check absorb seconds of skew.
- [Binance ledger window per symbol, Bybit all symbols, rate limits] → checks are merged per query and paced by the existing limiter; nothing is sent without keys, such rows stay due.
