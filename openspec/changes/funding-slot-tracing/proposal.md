## Why

A pair's four orders are known the moment it is flat, but its funding fees are not: the exchanges publish the ledger row minutes after the settlement (measured on the demo hosts, one sample: Binance 2.5–3.5 min, Bybit 3.5–4.5 min). Today a pair waits 10 minutes after the close and is then recorded `INCOMPLETE` for good; a fetch that never ran (machine asleep, network down: NMR pair `ui-1791344628622-1-nmrusdt`, settlement 12:00, finalized INCOMPLETE at 12:21, no retry afterwards) is never repaired. The ledger rows carry no `pair_id`; they are attributed by a time window every time a PnL is computed.

## What Changes

- A persistent **funding slot** per (pair, leg, settlement time) that the position was held through, with an explicit staged tracing state machine: `TRACING_1` (check every 5 min up to settlement+30 min) → `TRACING_2` (every 10 min up to +60 min) → `SETTLED`, or `MANUAL_CHECK` with a reason (`NOT_FOUND` / `FETCH_FAILED`).
- Slots live in the database, so asleep/offline/restarted periods are caught up on the next start; checks are driven by elapsed time, and a failed query never counts as "checked, nothing there".
- When a slot is found, the match is stored (`pair_id`, leg, settlement time, amount, exchange ledger id, first-seen time) and a `PAIR_FUNDING_LINKED` event is appended; the PnL is recomputed from the linked rows. The first-seen delay is recorded so the 5/10/30/60 minute constants can be calibrated.
- A settlement whose funding rate is exactly 0 may have no ledger row: if the public funding-rate history shows rate 0 at that time the slot settles with amount 0 (`ZERO_RATE`), so "no fee" is a recorded fact, not a gap.
- All three exchanges (Binance, Bybit, OKX) are covered.
- `MANUAL_CHECK` is not final: a late row still settles the slot (user "re-query" action, plus an hourly sweep for 3 days).
- Existing `INCOMPLETE`/finalized pairs with missing settlements are put back into tracing once (backfill).
- The positions page shows the tracing state per pair and a re-query action.

Out of scope (own changes later): the consolidated four-order trace record (`pair-order-trace`) and the history detail page (`pair-history-detail`).

## Capabilities

### New Capabilities
- `funding-slot-tracing`: persistent funding slots, staged tracing, linking to `pair_id`, catch-up, manual check, backfill.

### Modified Capabilities

(none — the earlier funding-pnl change is an unarchived change, its behaviour is extended in design.md)

## Impact

- `app/src/store/schema_v3.sql`, `store/db.rs` (migration 3, slot API), `funding/` (new `slots.rs`, `runner.rs`, `pnl_record.rs`), `engine/actor/pnl_gate.rs` (no longer waits for funding beyond the existing window), `ui/` (status text + re-query command), `app/src/store/events` (new event type).
