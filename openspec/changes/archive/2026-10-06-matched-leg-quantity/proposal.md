## Why

目前兩腿各自以「名目本金 ÷ 該所價格」並以該所步長向下取整計算數量，兩所價格與步長不同，所以開出的多空幣數不一致（例如 Binance 0.019 BTC、Bybit 0.0191 BTC），從一開始就有不平衡曝險，也讓 `max_leg_imbalance_pct` 的判定多了一個非成交造成的變因。策略改為：**兩腿固定為同一個幣數**，名目本金（例：200 保證金 × 5 倍 = 1,000 USDT）只當作上限。

## What Changes

- 新增「雙腿共同數量」計算（core，純函式）：
  - 共同步長 = 兩所步長（以幣為單位；OKX 為 `lotSz × ctVal`）與預設精度 `0.000001`（1e-6）的最小公倍數，確保同一數量在兩所都合法。
  - 最小量 = 兩所 `min_qty`（以幣為單位）的較大者。
  - 數量 = `名目本金 ÷ max(多腿價格, 空腿價格)` 向下取整到共同步長 → 兩腿的成交價值都 ≤ 名目本金，且是在該步長下最接近的值。
  - 低於最小量回傳「低於最小下單量」，兩腿都不送。
- engine 送單（Node 1）改用共同數量：兩腿送出同一幣數（OKX 換算為張數，必為整數倍 `lotSz`）。
- 交易單頁的多空數量改顯示共同數量，並顯示兩腿的預估價值（≤ 名目本金）。
- 合約設定頁的試算在逐所數量之外，新增 Binance↔Bybit 的共同數量與預估價值。
- **BREAKING（行為）**：兩腿數量不再各自取最大值，單腿價值可能比以前略低。

## Capabilities

### New Capabilities
（無）

### Modified Capabilities
- `quantity-precision`: 新增「配對開倉的兩腿使用同一數量」需求；既有逐腿取整規則保留給單腿下單（手動下單）。

## Impact

- `core/src/quantity.rs`：新增 `matched_quantity`（與 `MatchedLeg` 輸入），`core/tests/quantity_precision.rs` 測試。
- `app/src/engine/fill.rs`（`plan_submit`）、`app/src/engine/actor.rs`（呼叫處不變或微調）。
- `app/src/ui/vm/staged_orders.rs`、`app/src/ui/vm/contract_settings.rs` 與對應測試。
- 手動下單（單腿）不受影響。
