## ADDED Requirements

### Requirement: 手動下單頁標示為除錯工具並與標準流程隔開

手動下單頁 SHALL 在側邊欄以分隔線與標準七頁隔開，頁面頂部 SHALL 以警示樣式標示「除錯工具，非標準流程」，並說明單腿下單會造成未避險曝險、標準流程為「掃幣 → 交易單」。
頁面上關於環境的說明 SHALL 依目前 `execution_mode` 如實顯示：`SIMULATION` 時說明訂單由模擬器成交、不會送到任何交易所；`EXCHANGE_DEMO` 時說明將對 demo / testnet 帳戶真實下單。
頁面 SHALL NOT 在 `EXCHANGE_DEMO` 下顯示「無外部請求」或「無真實帳戶」之類不實的說明。
頁面 SHALL 只提供 Binance 與 Bybit 兩個交易所的面板；OKX 不提供下單。
`allowed_exchanges` 不包含某交易所時，該所面板 SHALL 被禁用並說明原因。

#### Scenario: 模式說明如實

- **WHEN** `execution_mode` 為 `EXCHANGE_DEMO`
- **THEN** 頁面顯示「將對 demo / testnet 帳戶真實下單」，且不顯示「無外部請求」

#### Scenario: 被停用的交易所

- **WHEN** `allowed_exchanges` 只包含 Binance
- **THEN** Bybit 面板被禁用，並顯示「此交易所未在 allowed_exchanges 中」

#### Scenario: 沒有 OKX 面板

- **WHEN** 開啟手動下單頁
- **THEN** 只出現 Binance 與 Bybit 兩個面板

### Requirement: 手動下單依 execution_mode 決定執行器且只走 engine 的單一路徑

手動下單頁的送出與撤單 SHALL 一律以 engine 的 Command 執行，由 engine 依目前的 `execution_mode` 選擇執行器（與已合併的 `engine-simulation` spec「SIMULATION 下的手動下單 → SimulatedExecutor」一致）：`SIMULATION` 下 SHALL 送往 `SimulatedExecutor`（不送到任何交易所），結果與事件 SHALL 標示為模擬；`EXCHANGE_DEMO` 下 SHALL 送往 demo 執行器。
頁面 SHALL NOT 持有或呼叫任何交易所 client；系統 SHALL 只有一條下單路徑。
頁面 SHALL 提供 `reduce_only` 選項；kill switch 已啟動或系統處於停機狀態時，非 reduce-only 的送出 SHALL 被禁用並說明原因，reduce-only 的送出 SHALL 仍可進行（`engine-simulation` D5：reduce-only 只會減少曝險）。
模式於頁面開啟期間改變時，環境說明與確認視窗的目標環境 SHALL 即時更新。

#### Scenario: SIMULATION 下送往模擬器

- **WHEN** `execution_mode` 為 `SIMULATION`，使用者完成確認並送出
- **THEN** engine 恰好收到一個單腿下單命令，由 `SimulatedExecutor` 成交，結果標示為模擬，沒有任何交易所請求

#### Scenario: EXCHANGE_DEMO 下送出經由 engine

- **WHEN** `execution_mode` 為 `EXCHANGE_DEMO`，使用者完成確認並送出
- **THEN** engine 恰好收到一個單腿下單命令，頁面本身沒有任何直接的交易所呼叫

#### Scenario: kill switch 啟動

- **WHEN** kill switch 為啟動狀態，且未勾選 `reduce_only`
- **THEN** 「Submit Order」被禁用並顯示「緊急停止中」；勾選 `reduce_only` 後可送出

#### Scenario: 開啟期間切換模式

- **WHEN** 頁面開啟時 `execution_mode` 由 `EXCHANGE_DEMO` 改為 `SIMULATION`
- **THEN** 環境說明立即改為「SIMULATION：由模擬器成交，不會送到交易所」

### Requirement: 單腿下單須取整、確認並如實顯示結果

單腿下單 SHALL 提供 Symbol、Side（BUY 或 SELL）與 Quantity 輸入，型別為市價單。
送出前 SHALL 以 `quantity-precision` 將數量向下取整並顯示取整後的數量；取整後低於最小下單量 SHALL 顯示錯誤且不送出。
送出 SHALL 先顯示確認視窗，列出交易所、標的、方向、取整後數量與估計 Notional，並警示「單腿下單不會自動建立對腿」。
結果 SHALL 如實顯示：成功時顯示交易所回報的 order id 與延遲；失敗時顯示交易所回報的錯誤。
每次送出與其結果 SHALL 寫入不可變事件（含送單前的 `ORDER_SUBMIT_ATTEMPT`）。

#### Scenario: 數量取整

- **WHEN** 使用者輸入 Quantity 0.0014、`step_size` 為 0.001
- **THEN** 確認視窗顯示取整後數量 0.001

#### Scenario: 低於最小下單量不送出

- **WHEN** 使用者輸入的 Quantity 取整後低於 `min_qty`
- **THEN** 顯示「低於最小下單量」，engine 收到 0 個命令

#### Scenario: 確認前不送出

- **WHEN** 使用者按「Submit Order」但尚未在確認視窗確認
- **THEN** engine 收到 0 個命令

#### Scenario: 交易所拒絕訂單

- **WHEN** 交易所回報拒單錯誤
- **THEN** 頁面顯示該錯誤原文，並有一筆對應的失敗事件被寫入

### Requirement: 撤單以 order id 為準

撤單 SHALL 以交易所、Symbol 與 Order ID（本系統送單時的 `client_order_id`）為輸入，SHALL 透過 engine 的 Command 執行，由目前 `execution_mode` 的執行器處理（`SIMULATION` 下為模擬器）。
Order ID 為空時 SHALL 禁用撤單；交易所回報找不到該訂單或已成交時，SHALL 如實顯示交易所的回應，SHALL NOT 顯示成功。
撤單結果 SHALL 寫入不可變事件。

#### Scenario: 空的 Order ID

- **WHEN** Order ID 欄位為空
- **THEN** 「Cancel」被禁用

#### Scenario: 訂單已成交無法撤銷

- **WHEN** 交易所回報該訂單已成交而無法撤銷
- **THEN** 頁面顯示交易所的回應，且不顯示撤單成功
