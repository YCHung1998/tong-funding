## ADDED Requirements

### Requirement: FundingObservation 的欄位與雙時間戳

每筆 `FundingObservation` SHALL 包含：`exchange`、`symbol`、`funding_rate`、`funding_interval_secs`、`next_funding_time`、
`mark_price`、`volume_24h_quote`、`exchange_timestamp`、`observed_at`、`data_status`。
`exchange_timestamp`（交易所資料產生時間）與 `observed_at`（本系統收到時間）SHALL 是兩個獨立欄位，不得合併。
所有費率與價格 SHALL 使用 Decimal，不得使用浮點數。

#### Scenario: 兩個時間戳分開保存

- **WHEN** 建立一筆觀測，交易所時間為 T，本機收到時間為 T+0.4 秒
- **THEN** 兩個欄位各自保留原值，不被覆寫或合併

### Requirement: Funding 週期必須由 API 資料推導，查不到時標為 DATA_ERROR

系統 SHALL 以各交易所的資料推導 `funding_interval_secs`：
Binance 使用 `fundingIntervalHours` 乘以 3600；
Bybit 使用 `fundingInterval`（單位：分鐘）乘以 60；
OKX 使用 `nextFundingTime − fundingTime`（毫秒）換算為秒。
推導結果若缺失、為零或為負，該觀測的 `data_status` SHALL 為 `DATA_ERROR`，系統 SHALL NOT 以 8 小時代替。

#### Scenario: 三所週期推導

- **WHEN** Binance 回報 `fundingIntervalHours = 4`、Bybit 回報 `fundingInterval = 480`、OKX 回報 `fundingTime = 1791187200000` 且 `nextFundingTime = 1791216000000`
- **THEN** 週期依序為 14400 秒、28800 秒、28800 秒

#### Scenario: 查不到週期時不猜測

- **WHEN** 某標的在 Binance 的週期資料缺失
- **THEN** 該觀測的 `data_status` 為 `DATA_ERROR`，且 `funding_interval_secs` 不被填入 28800

### Requirement: 資料狀態與過期判定

`data_status` SHALL 為 `LISTED`、`NOT_LISTED`、`DATA_ERROR`、`STALE` 之一。
當「目前時間 − `observed_at`」大於 `stale_data_threshold_ms` 時，該觀測 SHALL 被判定為 `STALE`。
過期判定 SHALL 以呼叫端注入的目前時間計算，不得在函式內讀取系統時鐘。

#### Scenario: 超過門檻即為過期

- **WHEN** `stale_data_threshold_ms` 為 5000，觀測的 `observed_at` 距注入的目前時間為 5001 毫秒
- **THEN** 判定為 `STALE`

#### Scenario: 剛好等於門檻不算過期

- **WHEN** 距離恰為 5000 毫秒
- **THEN** 不判定為 `STALE`

### Requirement: 有效狀態須同時檢查一致性與過期

取得觀測的「有效狀態」時，系統 SHALL 先檢查一致性：`data_status` 為 `LISTED` 但沒有有效的 funding 週期（缺失、零或負）者，有效狀態 SHALL 為 `DATA_ERROR`，不得信任儲存的狀態欄位。
其次才套用過期判定。一致性檢查 SHALL 不依賴觀測是經由建構函式、欄位指定或反序列化產生。

#### Scenario: 繞過建構子造出的不一致觀測

- **WHEN** 以欄位指定產生 `data_status = LISTED` 但 `funding_interval_secs` 為空的觀測
- **THEN** 其有效狀態為 `DATA_ERROR`

### Requirement: 8h 等效 rate 僅供顯示

系統 SHALL 提供「8h 等效 rate」函式，定義為 `funding_rate × 28800 ÷ funding_interval_secs`。
此值 SHALL 僅用於畫面顯示與排序參考，SHALL NOT 作為 Net Edge 或達標判定的輸入。

#### Scenario: 4 小時週期換算

- **WHEN** rate 為 0.0005、週期為 14400 秒
- **THEN** 8h 等效 rate 為 0.0010

### Requirement: 配對的結算時間取兩腿較早者

對一組 (long 腿, short 腿)，結算時間 `T` SHALL 為兩腿 `next_funding_time` 的較小值。
只有 `next_funding_time` 等於 `T` 的腿才被視為「會在 T 結算」。

#### Scenario: 週期不同時只有一腿結算

- **WHEN** long 腿下次結算為 08:00、short 腿下次結算為 04:00
- **THEN** `T` 為 04:00，且只有 short 腿會在 `T` 結算

#### Scenario: 結算時間相同則兩腿皆結算

- **WHEN** 兩腿下次結算皆為 08:00
- **THEN** `T` 為 08:00，兩腿皆會結算
