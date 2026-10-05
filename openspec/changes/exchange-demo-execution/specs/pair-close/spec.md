## ADDED Requirements

### Requirement: 平倉使用 reduce-only 且數量取自交易所回報的持倉

平倉 SHALL 先向交易所重新查詢該配對兩腿的實際持倉，數量 SHALL 以 core 的「由交易所持倉建構」方式取得，SHALL NOT 使用配對記錄的目標數量或重新取整。
平倉訂單 SHALL 帶 reduce-only 旗標，方向 SHALL 由該腿實際持倉的正負決定（多單賣出、空單買進），SHALL NOT 由配對記錄的方向推斷。
每筆平倉訂單 SHALL 同樣先落地意圖並帶 `client_order_id`。

#### Scenario: 以實際持倉平倉

- **WHEN** 配對記錄的 long 數量為 0.020，但交易所回報該腿持倉為 0.019
- **THEN** 平倉數量為 0.019，方向為賣出，訂單帶 reduce-only

#### Scenario: 空單平倉方向

- **WHEN** 交易所回報某腿持倉為 −0.019
- **THEN** 平倉訂單為買進、數量 0.019、帶 reduce-only

#### Scenario: 實際持倉與記錄不符時留下紀錄

- **WHEN** 實際持倉數量與配對記錄的成交量不同
- **THEN** 仍以實際持倉平倉，並寫入記錄兩個數字的事件

### Requirement: 腿已無持倉時不送單且記錄

平倉時若某腿持倉已為 0（例如被強平或使用者已手動平掉），系統 SHALL NOT 對該腿送出訂單，SHALL 記錄該腿已無持倉的事件，並繼續處理另一腿。

#### Scenario: 一腿已被強平

- **WHEN** 平倉時 long 腿持倉為 0、short 腿持倉為 −0.019
- **THEN** 只對 short 腿送出一筆 reduce-only 買單，並記錄 long 腿已無持倉

### Requirement: 兩腿並行平倉，單腿失敗轉人工

兩腿的平倉訂單 SHALL 並行送出，並與開倉相同地記錄 `request_sent_at`、`ack_at`、`latency_ms` 與結果分類。
任一腿平倉失敗或結果未知時，配對 SHALL 轉為 `PARTIAL_FAILURE` 並觸發警示，SHALL NOT 自動重試；已成功那一腿的資料 SHALL 保留。
人工再次要求平倉時，系統 SHALL 重新查詢持倉，因此只會處理仍有持倉的腿。

#### Scenario: 一腿平倉被拒

- **WHEN** short 腿的平倉訂單被交易所拒絕、long 腿平倉成功
- **THEN** 配對為 `PARTIAL_FAILURE` 並觸發警示，沒有任何自動重試

#### Scenario: 人工再次平倉只處理剩餘腿

- **WHEN** 使用者對上述配對再次要求平倉，此時 long 腿持倉為 0
- **THEN** 只對 short 腿送出一筆 reduce-only 訂單

### Requirement: FINALIZED 須確認兩腿持倉為 0 且無未成交委託

平倉訂單成交後，系統 SHALL 向交易所重新查詢兩腿持倉與該標的的未成交委託；兩腿持倉皆為 0 且兩個交易所上該標的皆無未成交委託，才可提供已平倉確認並轉為 `FINALIZED`。
持倉更新可能延遲，因此確認 SHALL 於生效的 `order_timeout_seconds` 內重試查詢；逾時仍不為 0 或無法查詢時，配對 SHALL 轉為 `PARTIAL_FAILURE` 並觸發警示。

#### Scenario: 兩腿皆為 0

- **WHEN** 重新查詢顯示兩腿持倉為 0 且無未成交委託
- **THEN** 配對轉為 `FINALIZED`

#### Scenario: 仍有未成交委託

- **WHEN** 兩腿持倉為 0，但某交易所上該標的仍有未成交委託
- **THEN** 不轉為 `FINALIZED`，逾時後轉為 `PARTIAL_FAILURE` 並觸發警示

#### Scenario: 持倉更新延遲

- **WHEN** 第一次查詢某腿持倉尚未歸零、第二次查詢（仍在逾時內）已為 0
- **THEN** 以第二次結果完成確認，配對轉為 `FINALIZED`

### Requirement: 手動下單頁的平倉也走同一條路徑

手動下單頁與人工平倉 SHALL 經由同一個 `Executor` 與同一套 reduce-only 規則；在 `SIMULATION` 下 SHALL 送往 `SimulatedExecutor`，在 `EXCHANGE_DEMO` 下 SHALL 送往真實執行器。
手動送出的減倉訂單 SHALL 以 `reduce_only` 為真，使其 `opens_exposure()` 為假。

#### Scenario: SIMULATION 下手動平倉

- **WHEN** `execution_mode` 為 `SIMULATION`，使用者在手動下單頁送出 reduce-only 平倉
- **THEN** 訂單由 `SimulatedExecutor` 處理，沒有任何交易所請求
