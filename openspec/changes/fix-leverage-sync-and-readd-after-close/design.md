## Context

- Leverage today: contract template (`config.contract_template.leverage`, default 5) → `NewPreparedPair.entry` → `pairs.entry_json.scan.leverage` → `engine/node0.rs` (`margin_needed = notional / leverage`, `max_leverage` check). The executor (`exchange/execution/executor.rs`) only checks position mode before an order; `OrderRequest` has no leverage field. Observed: snapshot 5, positions 20× (Binance) / 10× (Bybit).
- Scanner eligibility: `ui/vm/candidates.rs::staged()` returns true for every non-terminal state, including `CLOSING`, `PARTIAL_FAILURE`, `IMBALANCED`, `UNRESOLVED`. `CLOSING` lasts until the funding PnL is settled (`pnl_gate`, up to the settlement time + retry window), and the locked states last until the user acts.
- The store only enforces uniqueness for `PREPARED` per symbol (`uniq_prepared_symbol`), so a new pair on a symbol with a flat, unfinished pair is not blocked below the UI.

## Goals / Non-Goals

**Goals:**
- Both legs of a pair open at the leverage stored in the pair's entry snapshot.
- A symbol whose previous pair has no exposure left can be ticked again in the scanner.

**Non-Goals:**
- Changing existing positions' leverage, margin mode, or close orders.
- Manual-order-page orders (they have no leverage input; account default stays). Reported as follow-up.
- Finalizing PARTIAL_FAILURE pairs automatically (the user's "confirm closed" flow stays as is).
- Funding-PnL attribution changes (see Risks).

## Decisions

1. **Leverage travels on `OrderRequest` as `Option<Decimal>`; the executor applies it.** `Some` only for opening legs of a pair; `None` for close and manual orders and in simulation (the simulator ignores it). Alternative: a separate `Executor::prepare` trait method called by the actor — rejected: a second call site per leg and an ordering race with `spawn_submit`; carrying it on the request keeps "one order path" and the "not sent = certain rejection" semantics.
2. **Set leverage right before each opening submit, no cache.** Both calls are idempotent (Binance returns 200; Bybit answers retCode 110043 "not modified", treated as success). A cache would hide an externally changed leverage. Cost: one extra signed request per opening leg.
3. **Failure policy: any other failure ⇒ order not sent** (`SubmitClass::Rejected { code: "not_sent" }`, same as the position-mode guard); the existing pair logic turns that into `ONE_LEG_SUBMIT_FAILED` / `BOTH_SUBMITS_FAILED`, never an unknown result. A leverage failure never leaves an order on the exchange.
4. **Leverage conversion:** Binance takes an integer 1–125; Bybit takes a string. A non-integer or out-of-range leverage is `not_sent` locally (no request). Both sides send the same integer, so the legs match by construction.
5. **Flat detection for the scanner without new queries:**
   - `CLOSING` + flat confirmed: add `PairView.flat_confirmed: bool`, true exactly while `flows[pair].pnl_wait_since.is_some()` (set at `CLOSE_CONFIRMED`, cleared when finalized). The UI reads the engine's own verified-flat fact instead of guessing.
   - Locked states: use the UI snapshot's existing `leg_accounts[(simulated, exchange)]` (positions and open orders already fetched for the staged-orders/positions pages). Not staged only if for **both** legs the position list and open-order list are `Ok`, complete, and contain nothing on that symbol. Any `Err`/incomplete list ⇒ still staged (never treat "unknown" as flat).
   The account read must also be fresh (45 s = three 15 s polls); an older read is "unknown".
   Alternative: let the engine re-query flatness on demand — rejected as heavier, and the snapshot data suffices; the existing "confirm closed" command remains the authoritative way to leave a locked state.
6. **A flat-confirmed `CLOSING` pair frees its `max_concurrent_pairs` slot** (`count_open_pairs` input in the actor skips `CLOSING` pairs that `awaiting_pnl`). Found while writing the engine test: without it the scanner would let the user add the symbol and Node 0 would then BLOCK it with `RiskLimits` once enough closed pairs wait for PnL (max 5 by default). Locked-but-flat pairs still occupy a slot: the engine has no account read at that point; the user can finish them with "confirm closed".
7. **The old pair is untouched** (state, alert banner, PnL wait keep running); only the scanner's "already staged" rule changes.

## Risks / Trade-offs

- [Exchange rejects leverage above the symbol's bracket max (Binance -4028 / Bybit 110013)] → order not sent, reason shown in the pair's failure detail; pretrade `max_leverage` still applies first.
- [Changing leverage on a symbol that already holds a position (e.g. a flat-looking pair whose leg actually has another position)] → exchanges reject or adjust margin; failure ⇒ not sent. Re-add is allowed only when no position exists on the symbol for the pair's legs.
- [A new pair on the same symbol while the old one still waits for funding PnL may mix funding rows] → `assemble` already attributes ledger rows by per-pair open/close windows and flags overlaps as ambiguous; pinned by `pnl_record_a_later_pair_on_the_same_symbol_does_not_change_the_earlier_pairs_pnl` (a characterization test of existing behaviour, green before and after).
- [Both legs submit in parallel; if only one leg's leverage request fails, that leg is `not_sent` while the other leg's order may already be filled → existing `ONE_LEG_SUBMIT_FAILED` → `PARTIAL_FAILURE`] → same handling as any one-leg rejection; 5× is accepted by both exchanges on normal symbols. A pre-flight "set both leverages, then send both orders" would remove it; left as a follow-up (needs an engine-level step before `send_set`).
- [Manual-page orders are not covered] → they have no leverage input and keep the account's value; follow-up if wanted (e.g. use the contract template leverage).
- [Endpoint shapes are UNVERIFIED on the demo hosts] → built from the public API docs like the existing order requests; replay/contract tests pin the request shape and `live_probe` can exercise it.
- [`OrderRequest` literal sites change in ~15 places] → mechanical `leverage: None`; compile errors find them all.
- [`ORDER_LATENCY` of an opening leg now includes the leverage request's round trip, because the engine times `executor.submit`] → entries start at T−10 s so one extra round trip (~100–200 ms) is tolerable; the replay tests script instant leverage replies so their latency assertions are unchanged. A pre-flight step (see the partial-leg risk) would also keep this metric clean; follow-up.
