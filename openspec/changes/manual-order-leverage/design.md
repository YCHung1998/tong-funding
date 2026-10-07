## Context

Pairs already carry `OrderRequest.leverage` and the executor aligns the exchange (order-leverage-sync); caps come from `AccountView::max_leverage` and the `leverage_cap` view-model (symbol-leverage-cap). The manual page builds `ManualOrder { exchange, symbol, side, quantity, reduce_only }` → `Actor::manual_order` → `OrderRequest { leverage: None, .. }`.

## Decisions

1. **`ManualOrder.leverage: Option<Decimal>`**, filled by the page for opening orders. The engine forwards it only when `!reduce_only` and `Some`; `Some(<=0)` is refused with a reply (never sent). `None` keeps the old behaviour for programmatic callers.
2. **Field default**: loaded once from the contract settings leverage in `load_forms` (the place that has a `Window`), then user-editable. Alternative (always follow the template) rejected: the page is a debug tool and the user may test another value.
3. **Cap check**: reuse `leverage_cap` with a one-leg variant. The notional is the page's estimated notional; the cap request uses it rounded up to the next 1000 USDT so the cached reading does not change with every price tick. A reading is evidence when it was taken for an **equal or larger** notional (Binance caps only shrink as the notional grows, so a larger-notional reading is conservative); this also relaxes the staged/candidate rule from "same notional" to "not smaller".
4. **Rules**: known cap < leverage ⇒ submit disabled (any mode); unknown ⇒ disabled in EXCHANGE_DEMO, informational in SIMULATION; reduce-only ⇒ no leverage rule. Engine-side the executor's set-leverage refusal remains the last backstop; a single leg has no partial-pair risk, so no extra engine gate.
5. **Confirmation** shows `槓桿 N×` and the cap line.

## Risks

- [A conservative reading can show "exceeds" when the exact notional's cap is higher] → the reading is refreshed every 20 s with the rounded-up notional; the user can lower the quantity.
- [Changing leverage on a symbol with an existing position] → the exchange may refuse; then the order is not sent and the reason is shown.
