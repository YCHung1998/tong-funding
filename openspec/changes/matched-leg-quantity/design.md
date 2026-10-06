## Context

- `engine/fill.rs::plan_submit` 對兩腿各呼叫 `size_leg`（`Quantity::from_notional` / `okx_contracts`），各自以自己的價格與步長取整；`staged_orders.rs` 與 `contract_settings.rs` 透過 `leg_quantity` 做同樣的逐腿試算。
- `LotSize { step_size, min_qty }` 為該所下單單位（OKX 為張數）；`OrderRules.okx_ct_val` 為每張幣量。
- 成交後的不平衡判定（`fill_decision`）以兩腿 base 數量比較；共同數量讓「要求數量」本身就相同。

## Goals / Non-Goals

**Goals:** 兩腿同一幣數；在兩所都合法；價值 ≤ 名目本金且最接近。

**Non-Goals:** 交易所的最小名目金額（`minNotional`，目前規則未載入）；手動下單（單腿）；改變名目本金 / 槓桿的設定方式。

## Decisions

1. **共同步長 = LCM(step_L, step_S, 1e-6)**：以兩值的小數位數放大成整數求 gcd/lcm，再縮回 Decimal，精確無浮點誤差。預設精度 1e-6 是使用者指定的最細單位；任何一所的步長較粗時以較粗者為準。
2. **用兩腿中較高的價格計算**：`qty = floor(N / max(pL, pS))`，保證兩腿價值都 ≤ N；這是滿足「≤ 名目本金」下最大的共同數量。
3. **core 新增 `matched_quantity(notional, long: MatchedLeg, short: MatchedLeg) -> Result<MatchedQuantity, QuantityError>`**，`MatchedLeg { price, lot, ct_val: Option<Decimal> }`；回傳 `base_qty`、`common_step`、兩腿的下單 `Quantity`（OKX 為張數）。`Quantity` 仍只能由取整建構。
4. **engine `plan_submit` 改呼叫 `matched_quantity`**；失敗時兩腿都不送（沿用 `Abort`）。
5. **UI**：交易單頁兩腿顯示同一數量與各自預估價值；合約設定頁保留逐所試算（單腿參考），另加一列 Binance↔Bybit 共同數量。

## Risks / Trade-offs

- [共同步長較粗時，價值可能明顯低於名目本金（例如步長 1 的低價幣）] → 顯示預估價值，使用者可見差距；仍符合「≤ 且最接近」。
- [LCM 放大成整數可能溢位] → 以 i128 計算；步長小數位數 ≤ 28，實務上遠低於上限；溢位時回傳錯誤而非 panic。
