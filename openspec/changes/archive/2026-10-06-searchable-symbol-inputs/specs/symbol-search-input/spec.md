## ADDED Requirements

### Requirement: 標的輸入框提供可搜尋的候選清單

合約設定「試算標的」、手動下單 Symbol、撤單 Symbol 與風控 `allowed_coins` SHALL 在使用者輸入時顯示候選清單，清單 SHALL 可捲動並可用滑鼠或鍵盤上下鍵選取。
過濾 SHALL 不分大小寫、以子字串比對；候選 SHALL 以字母序排列。

#### Scenario: 打到一半即可選

- **WHEN** 使用者在手動下單 Symbol 輸入 `pep`
- **THEN** 清單只列出包含 `PEP` 的 Symbol（如 `1000PEPEUSDT`），選取後輸入框值為 `1000PEPEUSDT`

#### Scenario: 多選幣種

- **WHEN** 使用者在 `allowed_coins` 依序選取 `BTC` 與 `ETH`
- **THEN** 欄位值為 `BTC, ETH`，儲存後風控設定的 `allowed_coins` 為這兩個幣

### Requirement: 候選來源

試算標的與手動下單 Symbol 的候選 SHALL 取自最新行情快照中對應交易所的 Symbol（手動下單依目前選取的交易所；試算標的取所有交易所聯集）。
撤單 Symbol 的候選 SHALL 取自對應帳戶在該所的掛單 Symbol。
`allowed_coins` 的候選 SHALL 為行情快照中所有 Symbol 的基礎幣（去重）。
行情尚未載入時清單 SHALL 顯示「行情載入中」，且輸入框仍可自由輸入。

#### Scenario: 依交易所切換候選

- **WHEN** 手動下單選取的交易所由 Binance 改為 Bybit
- **THEN** Symbol 候選改為 Bybit 行情中的 Symbol

### Requirement: 允許不在清單中的值

使用者 SHALL 可以輸入並使用不在候選清單中的值；此時欄位旁 SHALL 顯示「不在目前行情清單中」提示，但 SHALL NOT 阻擋送出（既有的驗證流程照常進行）。

#### Scenario: 自由輸入

- **WHEN** 使用者輸入 `NEWUSDT` 而行情快照中沒有該 Symbol
- **THEN** 欄位值為 `NEWUSDT`，旁邊顯示「不在目前行情清單中」
