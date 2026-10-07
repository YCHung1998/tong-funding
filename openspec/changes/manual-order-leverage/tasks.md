## 1. Engine

- [x] 1.1 `ManualOrder.leverage`; actor forwards it on opening orders only, refuses `<= 0`; tests `flow_tests::a_manual_opening_order_*` (red shown by disabling the forwarding: `[(false,None),(true,None)]` vs `[(false,Some(5)),(true,None)]`)

## 2. View-model

- [x] 2.1 `leverage_cap`: a reading for an equal-or-larger notional is accepted; single-leg helpers; tests updated/added
- [x] 2.2 `ManualForm.leverage`, validation (whole number 1–125), cap rule, confirmation fields, command carries leverage; tests in `manual_order_tests.rs` for every scenario

## 3. Page

- [x] 3.1 Leverage input (filled once from the contract leverage), cap line, cap request with the notional rounded up to 1000, confirmation text

## 4. Verify

- [x] 4.1 Full `cargo test -p tong-funding -p tong-funding-core` green (1308 passed, 5 ignored); `openspec validate manual-order-leverage`
- [x] 4.2 The executor path a manual opening order takes (`OrderRequest.leverage` → set-leverage → order) is the one verified on the real demo hosts by `live_leverage_probe` (see `symbol-leverage-cap-check` 4.2); the engine hand-over is covered by the flow tests above
