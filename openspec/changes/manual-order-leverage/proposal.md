## Why

The manual order page sends single-leg market orders without any leverage: the exchange keeps whatever the account already has on that symbol (observed 20× on Binance and 10× on Bybit, not 1×). The user cannot see or choose the leverage, cannot judge whether it fits the symbol's exchange cap, and a manual leg opened next to a staged pair can end up at a different leverage than the pair. The page needs a leverage field that is actually applied and checked.

## What Changes

- Manual order form gains a **Leverage** field, filled once from the contract settings leverage and editable.
- Opening orders (not reduce-only) require a whole-number leverage 1–125; the field is ignored (and said so) for reduce-only orders.
- The exchange cap of the chosen symbol is shown (same lookup as staged orders) and a known cap below the leverage disables submit; an unknown cap disables submit in EXCHANGE_DEMO only.
- `ManualOrder` carries `leverage`; the engine puts it on the opening `OrderRequest`, so the executor sets it on the exchange before the order (same path as pairs); a refused leverage means the order is not sent.
- The confirmation dialog shows the leverage and the cap check.

## Capabilities

### New Capabilities
- `manual-order-leverage`: leverage input, cap check and application for manual opening orders.

### Modified Capabilities

(none)

## Impact

- `app/src/ui/vm/manual_order.rs` (+tests), `ui/trading_pages.rs` (field, request, prefill), `ui/vm/leverage_cap.rs` (single-leg check; reading accepted when taken for an equal or larger notional), `engine/command.rs` + `engine/actor.rs` (`ManualOrder.leverage`).
