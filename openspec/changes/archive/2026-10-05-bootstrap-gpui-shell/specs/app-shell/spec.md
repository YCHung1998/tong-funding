## ADDED Requirements

### Requirement: 側邊欄導覽包含 8 個頁面且順序固定

主視窗 SHALL 在左側顯示側邊欄，依序列出「總覽、掃幣、合約設定、交易單、持倉、風控設定、系統日誌」七個標準頁面，
其後以分隔線隔開，最後一項為「手動下單」，並 SHALL 以警示文字標示其為除錯工具、非標準流程。
每個項目 SHALL 同時顯示中文名稱與英文副標（例如「總覽 Dashboard」）。程式啟動後 SHALL 預設顯示「總覽」頁。

#### Scenario: 啟動後預設顯示總覽

- **WHEN** 使用者啟動程式
- **THEN** 視窗顯示側邊欄 8 個項目，且「總覽」為選取狀態、內容區顯示總覽頁佔位

#### Scenario: 點選項目切換頁面

- **WHEN** 使用者點選側邊欄的「持倉」
- **THEN** 內容區切換為持倉頁佔位，且側邊欄只有「持倉」呈現選取狀態

#### Scenario: 手動下單與標準流程視覺隔離

- **WHEN** 使用者查看側邊欄
- **THEN** 「手動下單」位於分隔線之下，並帶有「除錯工具，非標準流程」的警示標示

### Requirement: 標題列同時顯示 UTC 與 Taipei 時間

標題列 SHALL 同時顯示 UTC 與 Taipei（UTC+8）的日期與時間，並 SHALL 每秒更新一次。
兩個時鐘 SHALL 取自同一個系統時間來源，因此兩者的差值恆為 8 小時。

#### Scenario: 時鐘每秒更新

- **WHEN** 程式運行並經過 1 秒
- **THEN** 兩個時鐘顯示的秒數各自前進 1 秒，且 Taipei 時間恆等於 UTC 時間加 8 小時

### Requirement: 狀態列顯示模式、連線狀態與 kill switch 位置

狀態列 SHALL 顯示目前的執行模式徽章、每個交易所（Binance、Bybit、OKX）的連線狀態指示，以及 kill switch 狀態的預留位置。
在本 change 範圍內，連線狀態 SHALL 一律顯示為「未連線」，kill switch 位置 SHALL 顯示為「未啟用」。

#### Scenario: 首次啟動的預設模式為 SIMULATION

- **WHEN** 使用者首次啟動程式（尚無任何已儲存設定）
- **THEN** 狀態列的模式徽章顯示 `SIMULATION`，而不是任何可下單的模式

#### Scenario: 顯示 Demo/Testnet 標示

- **WHEN** 使用者查看標題列或狀態列
- **THEN** 畫面上有明確的「Demo / Testnet」標示，且不出現「LIVE」字樣

### Requirement: 純邏輯 crate 不得依賴 GPUI

`core` crate SHALL NOT 直接或間接依賴 `gpui` 或 `gpui-kit`，使 `cargo test -p core` 不需要連結 GPUI。

#### Scenario: core 的依賴樹不含 gpui

- **WHEN** 對 `core` crate 檢查其依賴樹
- **THEN** 依賴樹中不存在 `gpui` 或 `gpui-kit`

### Requirement: 效能基準驗證須在封存前完成並記錄

`app` SHALL 提供一個僅供開發使用的基準頁，渲染 528 列、可調更新頻率（Hz）的資料表，並量測幀時間。
量測結果（含更新頻率、p50、p95 與最大幀間隔、每幀 CPU 時間、機器型號）SHALL 記錄在本 change 的 `design.md`。
預算以當下實際的 vsync 週期為基準（不使用固定的 16.7 ms，因為系統電源狀態會改變螢幕更新率）：掉幀率（幀間隔超過 1.5 倍 vsync 週期者，間隔超過 250 毫秒視為視窗暫停而不計入）不超過 0.5%，且每幀 CPU 時間小於一個 vsync 週期。量測紀錄 SHALL 註明電源狀態。
若任一項超過預算，本 change SHALL NOT 被封存，直到 `design.md` 記錄了緩解決策。

#### Scenario: 基準頁量測並產出結果

- **WHEN** 開發者在基準頁選擇 2Hz 更新頻率並執行量測
- **THEN** 程式輸出 p50、p95 與最大幀間隔數值，且這些數值被寫入 `design.md` 的量測紀錄表

#### Scenario: 超出預算時阻擋封存

- **WHEN** 記錄的掉幀率超過 0.5%，或每幀 CPU 時間達到一個 vsync 週期以上，且 `design.md` 沒有緩解決策
- **THEN** 本 change 被視為未完成，不得執行封存
