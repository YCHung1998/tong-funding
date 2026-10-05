## Why

SIMULATION 只證明邏輯，不證明交易所真的接受這些請求；Python 版歷史上多次在真實 demo/testnet 才發現 API 假設錯誤。
這個 change 讓引擎在 `EXCHANGE_DEMO` 下對 Binance 與 Bybit 的 demo/testnet 帳戶**真實下單、撤單、平倉**，
並依使用者的決定處理單腿失敗：**完全人工**，系統只警示、不自動動倉位。

Python 版另有幾個已對照原始碼確認的缺陷在此一併修掉：兩支 client 都不帶 client order id；成交確認只看「該標的有沒有持倉」且容差 20%；
平倉沒有 reduce-only 且數量取自配對記錄而非交易所持倉；`requests` 類例外（逾時、斷線）沒有被分類，會讓配對卡在 `ORDER_SUBMIT` 而已成功的那腿訂單仍在交易所上；
`order_timeout_seconds` 只存在設定頁。

## What Changes

- Binance、Bybit 簽名下單、撤單與查單，每一單帶 `client_order_id`（Binance `newClientOrderId`、Bybit `orderLinkId`）；兩腿並行送出；記錄 request、ACK 時間與 latency。
- 送單結果分類（已接受 / 已拒絕 / 被限流 / 結果未知）；逾時與斷線一律是「結果未知」並以 `client_order_id` 查單，不得當成失敗或吞掉。
- 以 order id 比對的成交確認（含部分成交、不平衡計算），取代「該標的有無持倉、容差 20%」。
- 實作 `order_timeout_seconds`（生效值，雙腿取小）；逾時只撤銷自己送出且未成交的訂單並再查最終成交量，依 `next()` 轉狀態。
- 單腿失敗、不平衡、無法確認時：Snapshot 常駐警示（由配對狀態推導）、macOS 系統通知、事件記錄；**不自動補買、補賣或平倉**；人工出口只有「人工要求平倉」與「人工確認已平倉」（後者須重新查詢驗證）。
- 平倉使用 reduce-only，數量取自交易所回報的實際持倉（`Quantity::from_exchange_position`）；FINALIZED 須確認兩腿持倉為 0 且無未成交委託。
- 真實 demo 帳戶的小量驗證流程與驗證紀錄（由使用者在場執行）。
- OKX 不下單，只做公開行情。

## Capabilities

### New Capabilities
- `signed-order-execution`: 寫死的 demo/testnet 端點、`client_order_id`、持倉模式確認、結果分類、兩腿並行與 latency 記錄、查單與限定撤單、與 `SimulatedExecutor` 同一介面契約。
- `fill-confirmation`: 依 order id 的成交確認、不平衡計算、生效逾時、只撤自己未成交的單、零自動補單、限流下的輪詢。
- `partial-failure-alerting`: 警示三通道、由狀態推導的常駐警示、通知失敗不影響、警示期間不動倉位、兩條人工出口、依原因可統計。
- `pair-close`: reduce-only 平倉、數量取自交易所持倉、腿已無持倉、單腿失敗轉人工、已平倉確認、手動平倉同路徑。

### Modified Capabilities
<!-- 無 -->

## Impact

- 依賴 `engine-simulation`（`Executor` / `AccountView` 介面、對帳與警示契約）、`store-sqlite`（意圖、事件、Keychain）、`exchange-readonly-adapters`（持倉、委託、`serverTime`、限流退避）。
- `EXCHANGE_DEMO` 是唯一能真實下單的模式，端點仍寫死為 demo/testnet；真實執行器只由切換模式的工廠建立。
- 橫幅的繪製在 `ui-trading-pages`；本 change 只保證 Snapshot 的警示資料與通知。
- Python 版 demo 運行事件統計：`PAIR_PARTIAL_FAILURE` 47 筆、`PAIR_FINALIZED` 48 筆、`ORDER_SUBMIT_FAILED` 48 筆。
  唯讀檢視該檔後發現這 48 筆失敗中 46 筆是同一個 Bybit 進場、錯誤本文為字面上的 `rejected`（不像交易所真實回應，可能是測試替身寫入真實日誌，**未驗證**），
  本文可辨識為交易所真實回應的失敗只有 2 筆（Binance 進場 `-2027`、Bybit 平倉 `110090`）。因此 Python 版的數字不能直接當作完全人工警示頻率的基準，頻率須在本 change 的驗證階段重新量測並回報使用者。
- **前置條件（需在實作前確認，見 design.md Open Questions）**：`core` 的 `next()` 需有 `CLOSING → PARTIAL_FAILURE`、`FILL_MONITOR → IMBALANCED`，以及人工確認已平倉後的目標狀態；`exchange-readonly-adapters` 的持倉需以正負號表示方向（Bybit 的 `size` 本身無正負）。
