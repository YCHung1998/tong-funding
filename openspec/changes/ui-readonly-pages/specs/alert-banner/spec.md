## ADDED Requirements

### Requirement: 警示橫幅常駐於所有頁面

警示橫幅 SHALL 顯示在全部 8 個頁面的內容區上方（標題列之下），不論目前所在頁面。
橫幅 SHALL 列出所有目前成立的警示；沒有警示時 SHALL 不佔版面。
警示 SHALL 只有在其成立條件消失時才消失；嚴重度為「需人工處理」與「停機」的警示 SHALL 沒有關閉按鈕，SHALL NOT 可被使用者手動隱藏。
橫幅 SHALL 不依賴目前頁面的資料載入結果：頁面載入失敗或資料庫不可用時，橫幅仍 SHALL 顯示。

#### Scenario: 切換頁面橫幅仍在

- **WHEN** 存在一則成立中的警示，使用者從總覽切換到系統日誌
- **THEN** 橫幅仍顯示同一則警示

#### Scenario: 無警示不佔版面

- **WHEN** 沒有任何警示成立
- **THEN** 橫幅區域不佔版面高度

#### Scenario: 需人工處理的警示無法關閉

- **WHEN** 存在「需人工處理」警示
- **THEN** 該警示沒有關閉控制項，且只在條件消失後才移除

### Requirement: 警示類別、嚴重度與排序

警示 SHALL 為下列類別，依嚴重度由高到低排序顯示：
1. 需人工處理：任一配對的狀態為 `PARTIAL_FAILURE`、`IMBALANCED` 或 `UNRESOLVED`；
2. 停機：系統因失敗即封閉而停機（`durable-state`）或 kill switch 啟用；
3. 交易所斷線或未連線：某交易所的簽名帳戶來源為未連線，或其資料來源為已斷線；
4. 資料過期：某資料來源依 `feed-health` 的規則被判定為過期；
5. 限流與時鐘：某所處於限流退避、時鐘未校時，或校時偏移量過大。
每則警示 SHALL 標明來源（交易所或配對標的）、類別與持續時間或年齡。
警示 SHALL 由單一純函式根據輸入（來源健康狀態、系統旗標、配對狀態列表）產生，相同輸入 SHALL 產生相同輸出與相同順序。
同一類別、同一來源的警示 SHALL 只出現一則。

#### Scenario: 嚴重度排序

- **WHEN** 同時有「Bybit 資料過期」與「BTCUSDT 處於 PARTIAL_FAILURE」
- **THEN** 橫幅中「BTCUSDT PARTIAL_FAILURE」排在「Bybit 資料過期」之前

#### Scenario: 同一來源不重複

- **WHEN** 同一個來源連續兩次判定為過期
- **THEN** 橫幅中只有一則該來源的資料過期警示

#### Scenario: 相同輸入相同輸出

- **WHEN** 以相同的輸入呼叫警示產生函式兩次
- **THEN** 兩次的結果與順序完全相同

### Requirement: 需人工處理的警示要指出配對並可前往

需人工處理的警示 SHALL 顯示配對的標的、兩腿的交易所、狀態名稱與「需人工處理，系統不會自動補單或平倉」。
警示 SHALL 提供前往持倉頁的連結。
警示 SHALL 在該配對離開上述三個狀態之前持續存在；橫幅 SHALL 只提供導覽連結，SHALL NOT 內含任何會改變配對狀態或倉位的控制項。

#### Scenario: 單腿失敗的警示內容

- **WHEN** BTCUSDT（long 在 Binance、short 在 Bybit）的狀態為 `PARTIAL_FAILURE`
- **THEN** 橫幅顯示「BTCUSDT · Binance / Bybit · PARTIAL_FAILURE · 需人工處理，系統不會自動補單或平倉」與前往持倉頁的連結

#### Scenario: 狀態被人工事件移出後消失

- **WHEN** 該配對因人工確認已平倉而離開 `PARTIAL_FAILURE`
- **THEN** 該警示不再顯示

### Requirement: 資料過期與斷線警示使用來源層級的判定

「資料過期」與「交易所斷線」警示 SHALL 使用 `feed-health` 的來源層級判定（斷線即時、過期門檻為 `max(stale_data_threshold_ms, 3 × 預期更新週期)`），SHALL NOT 以每一列觀測各自的 1 秒門檻判定，以免輪詢型來源持續閃爍。
過期警示 SHALL 顯示來源名稱與自最後一次成功更新起的年齡（例如「Bybit 行情已 23 秒未更新」）。
健康狀態未知或無法取得時，SHALL 視為異常並顯示警示，SHALL NOT 視為正常。

#### Scenario: 輪詢來源在門檻內不警示

- **WHEN** Bybit 輪詢週期為 10 秒，最後一次成功更新在 12 秒前
- **THEN** 沒有 Bybit 資料過期警示

#### Scenario: 超過門檻警示並顯示年齡

- **WHEN** Bybit 輪詢週期為 10 秒（門檻 30 秒），最後一次成功更新在 31 秒前
- **THEN** 顯示「Bybit 行情已 31 秒未更新」

#### Scenario: WebSocket 斷線即時警示

- **WHEN** Binance WebSocket 回報斷線
- **THEN** 橫幅立即出現 Binance 斷線警示，不等待任何計時器

#### Scenario: 健康狀態未知

- **WHEN** 某來源的健康狀態尚未取得
- **THEN** 橫幅顯示該來源的警示，而不是不顯示

### Requirement: 停機警示顯示原因且不覆蓋資料

停機警示 SHALL 顯示停機原因（例如資料庫無法開啟、設定讀取失敗、kill switch 已啟用）與發生時間。
顯示停機警示 SHALL NOT 刪除、重設或覆蓋任何既有資料。
停機期間，頁面 SHALL 仍可唯讀地顯示已有的資料。

#### Scenario: 資料庫無法開啟

- **WHEN** 啟動時資料庫無法開啟
- **THEN** 橫幅顯示停機與原因，且沒有任何資料檔被修改

### Requirement: 資料新鮮度指示

每個依賴資料來源的頁面區塊 SHALL 顯示該來源的新鮮度指示，內容為：狀態（ONLINE、STALE、OFFLINE、RATE_LIMITED）、自最後一次成功更新起的年齡，以及下次輪詢倒數（輪詢型來源）。
新鮮度指示的狀態 SHALL 與橫幅使用同一個來源層級判定，兩者 SHALL 不會互相矛盾。
資料尚未載入時 SHALL 顯示「載入中」；載入失敗時 SHALL 顯示錯誤與年齡；兩者 SHALL 與「沒有資料」的空狀態明確區分。

#### Scenario: 指示與橫幅一致

- **WHEN** 某來源被判定為過期
- **THEN** 該區塊的指示顯示 STALE，且橫幅有對應警示

#### Scenario: 載入中與空狀態的區別

- **WHEN** 首次資料尚未取得
- **THEN** 顯示「載入中」，而不是「沒有資料」

#### Scenario: 載入失敗

- **WHEN** 首次載入失敗
- **THEN** 顯示錯誤訊息與嘗試的時間，而不是空白表格
