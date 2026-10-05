## ADDED Requirements

### Requirement: 時鐘由外部注入，引擎不得讀系統時鐘

引擎與排程器 SHALL 透過注入的 `Clock` 介面取得目前時間與 tick，SHALL NOT 在 `engine` 模組內直接呼叫系統時鐘（例如 `SystemTime::now`、`Instant::now`）。
只有程式進入點 SHALL 建立真實時鐘實作。排程器 SHALL 每 1 秒處理一次 tick。

#### Scenario: 測試以假時鐘驅動整輪流程

- **WHEN** 測試以假時鐘從「結算前 20 秒」推進到「結算後 20 秒」
- **THEN** 不需實際等待，進場與出場依序被觸發，且事件時間戳等於假時鐘的值

#### Scenario: 原始碼不含系統時鐘呼叫

- **WHEN** 以自動化測試掃描 `engine` 模組原始碼
- **THEN** 找不到對系統時鐘的直接呼叫（真實時鐘實作所在檔案除外）

### Requirement: 進出場時點以交易所時間校正

配對的結算時間 `T` SHALL 取兩腿 `next_funding_time` 的較早者，並 SHALL 於配對建立時固定保存。
進場時點 SHALL 為 `T` 之前 15 秒，出場時點 SHALL 為 `T` 之後 15 秒；比較用的「現在」SHALL 為本機時鐘加上該腿交易所的 `serverTime` 偏移。
偏移不可用時，進場 SHALL NOT 觸發（失敗即封閉）；出場 SHALL 仍可由人工觸發。

#### Scenario: 偏移使時點提前

- **WHEN** 本機時鐘比交易所慢 2 秒（偏移為 +2 秒）、`T` 為 12:00:00（交易所時間）
- **THEN** 本機時鐘在 11:59:43（交易所時間 11:59:45）時觸發進場

#### Scenario: 偏移不可用

- **WHEN** 進場時點到達但該交易所的 `serverTime` 偏移不可用
- **THEN** 進場不觸發，配對維持 `PREPARED` 並記錄事件說明原因

### Requirement: 進場只在結算前的視窗內有效

進場 SHALL 只在 `[T − 15 秒, T)`（以校正後時間計）內觸發；`T` 之後 SHALL NOT 進場，因為此時進場已拿不到該次結算的 funding。
程式停機或延遲導致錯過視窗時，該配對 SHALL 被取消（轉為 `CANCELLED`），SHALL NOT 補進場。

#### Scenario: 重啟時已過結算

- **WHEN** 程式重啟時，某 `PREPARED` 配對的 `T` 已過去
- **THEN** 該配對被取消並記錄「錯過進場視窗」事件，不呼叫任何 `Executor`

### Requirement: 基準價在進場前重新抓取且與送單前價格分開

引擎 SHALL 在進場時點之前一個固定提前量重新抓取基準價並記錄其 `observed_at`；進場時點的送單前檢查 SHALL 再重新抓取一次最新價格。
最新價格的 `observed_at` SHALL 嚴格晚於基準價的 `observed_at`；兩者 SHALL NOT 取自同一份未更新的快取。
基準價抓取失敗時，送單前檢查 SHALL 依 core 規則退回掃描當時價格，並記錄事件。

#### Scenario: 兩次價格各自抓取

- **WHEN** 配對經過基準價抓取與進場觸發
- **THEN** 假資料來源被呼叫兩次、各自回傳不同 `observed_at`，且送單前檢查使用的是第二次的值

#### Scenario: 快取未更新時被擋下

- **WHEN** 第二次抓取回傳的 `observed_at` 與基準價相同（資料來源沒有更新）
- **THEN** `DataFresh` 失敗，配對為 `BLOCKED`

### Requirement: Node 0 以最新資料與生效設定執行送單前檢查

Node 0 SHALL 呼叫 core 的送單前檢查（10 項具名檢查），輸入為剛抓取的最新資料與該配對的 `effective_for_pair` 生效設定（雙腿保守值合併）。
引擎內所有下單前判斷（價格漂移、資料過期門檻、槓桿、保證金、`order_timeout_seconds`、`max_leg_imbalance_pct`、Net Edge 門檻）SHALL 一律使用該生效設定，SHALL NOT 直接讀取全域值。
設定標為「不完整」時，Node 0 SHALL 判定 BLOCK 並指出缺少的欄位。

#### Scenario: 覆寫真的影響執行

- **WHEN** 全域 `max_leverage` 為 5、Bybit 覆寫為 4，配對為 long Binance、short Bybit 且槓桿為 5
- **THEN** Node 0 的 `Leverage` 失敗，配對為 `BLOCKED`

#### Scenario: 資料過期時阻擋

- **WHEN** 某腿價格的 `observed_at` 距目前 1,001 毫秒、生效 `stale_data_threshold_ms` 為 1000
- **THEN** `DataFresh` 失敗，配對為 `BLOCKED`，且沒有任何下單被送出

#### Scenario: 設定不完整

- **WHEN** 未填 `est_slippage_pct`
- **THEN** Node 0 回傳 BLOCK，原因指出缺少 `est_slippage_pct`，而不是以 0 計算

### Requirement: Node 1 數量一律經 Quantity，兩種模式相同

Node 1 SHALL 對每一腿以 `Quantity` 的取整函式計算下單數量（含 OKX 張數換算）；取整後低於 `min_qty` 時該腿 SHALL 判為失敗，SHALL NOT 以 0 或原數量送單。
此規則 SHALL 在 `SIMULATION` 與 `EXCHANGE_DEMO` 兩種模式相同。

#### Scenario: 低於最小下單量

- **WHEN** 某腿取整後數量低於 `min_qty`
- **THEN** 該腿失敗並依 core 的轉移規則進入對應狀態，`Executor` 沒有收到該腿的送單

### Requirement: 逾時使用生效的 order_timeout_seconds 並依雙腿成交分流

引擎 SHALL 實際讀取該配對生效的 `order_timeout_seconds`（雙腿取小）作為成交等待上限；Python 版只存在於設定頁、執行路徑沒有讀取的行為 SHALL NOT 重現。
逾時時，引擎 SHALL 依兩腿已成交量呼叫 core 的 `next()` 分流（皆未成交為 `CANCELLED`、一腿成交另一腿未完全成交為 `PARTIAL_FAILURE`、無法判定為 `UNRESOLVED`），SHALL NOT 產生任何自動補買、補賣或平倉。

#### Scenario: 設定值真的生效

- **WHEN** 生效的 `order_timeout_seconds` 為 7，假時鐘在兩腿送出後前進 7 秒而兩腿皆未成交
- **THEN** 在第 7 秒觸發逾時處理，而不是預設的 15 秒

#### Scenario: 一腿完成、另一腿 70%

- **WHEN** 逾時時 long 已 100%、short 僅 70%
- **THEN** 配對為 `PARTIAL_FAILURE`，且 `Executor` 沒有收到除原兩筆以外的任何送單

### Requirement: PREPARED 配對條件惡化時自動撤銷，停機時仍執行

`trigger_mode` 為 `AUTO` 時，引擎 SHALL 持續重新評估每個 `PREPARED` 配對；其 Net Edge 不再達標、任一腿 24h 成交量低於生效門檻，或任一腿所在交易所已不在 `allowed_exchanges` 時，SHALL 取消該配對並記錄原因。
此撤銷只移除曝險，SHALL 在 kill switch 停機期間照常執行。
此撤銷 SHALL NOT 影響任何已進入 `ORDER_SUBMIT` 之後狀態的配對。

#### Scenario: 停機中仍撤銷

- **WHEN** kill switch 已停機，且某 `PREPARED` 配對的一腿交易所被移出 `allowed_exchanges`
- **THEN** 該配對被取消並記錄原因

#### Scenario: 不碰已有曝險的配對

- **WHEN** 某腿所在交易所被移出 `allowed_exchanges`，而該配對已是 `RECONCILED`
- **THEN** 該配對狀態不變，到出場時點仍照常平倉

### Requirement: SIMULATION 完整走完進場至出場

`SIMULATION` 下一組配對 SHALL 走完完整流程：Node 0 檢查、進場、模擬成交、`RECONCILED`、出場、`FINALIZED`；SHALL NOT 在進場後直接跳到 `FINALIZED`。
`FINALIZED` 的「兩腿持倉為 0 且無未成交委託」確認 SHALL 以模擬持倉帳計算，不得省略。

#### Scenario: 完整一輪事件序列

- **WHEN** 以假時鐘跑完一輪進場至出場（`SIMULATION`、兩腿完整成交）
- **THEN** 事件序列依序包含 `PRE_TRADE_CHECK`、`ORDER_SUBMIT`、`FILL_MONITOR`、`RECONCILED`、`CLOSING`、`FINALIZED`，且 `FINALIZED` 前有已平倉確認事件
