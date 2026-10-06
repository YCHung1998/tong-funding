## 1. core（先寫失敗測試於 `core/tests/quantity_precision.rs`）

- [x] 1.1 `matched_quantity`：共同步長 LCM（含 1e-6）、最小量取大、以較高價格向下取整、低於最小量錯誤、OKX 換算張數；涵蓋 spec 五個情境
- [x] 1.2 LCM 溢位回傳錯誤（不 panic）

## 2. engine

- [x] 2.1 `fill.rs::plan_submit` 改用 `matched_quantity`，兩腿送出同一幣數；更新 / 新增 `fill.rs` 測試
- [x] 2.2 確認 actor 呼叫處與既有 flow / replay 測試在新數量下仍正確（必要時更新期望值）

## 3. UI

- [x] 3.1 交易單頁：兩腿顯示共同數量與各自預估價值；更新 `staged_orders_tests.rs`
- [x] 3.2 合約設定頁：新增 Binance↔Bybit 共同數量列；更新 `contract_settings_tests.rs`

## 4. 驗證

- [x] 4.1 `cargo test`（core 與 app 全套）綠燈
- [x] 4.2 實機（使用者）：交易單頁兩腿數量相同、價值 ≤ 名目本金（使用者實機確認 2026-10-06）
