## ADDED Requirements

### Requirement: 持倉頁顯示 Funding 收到

持倉頁 SHALL 在每一腿與每一組配對上顯示「Funding 收到」：自該腿開倉成交後、歸屬於該腿的 funding 流水累計金額（收到為正、支付為負），顏色 SHALL 使用 theme 的 funding rate 顏色語意函式（正為正向色、負為負向色、零為弱化色）。
尚未取得流水、取得失敗或金鑰不可用時 SHALL 顯示「—」與原因，SHALL NOT 顯示 0；並 SHALL 顯示該數值的最後更新時間。
價差未實現 PnL 欄 SHALL 標示為「價差，未含 funding 與手續費」，取代 Figma 的「PnL 未扣手續費與資金費」註記；配對卡 SHALL 另列已付開倉手續費與「進行中合計」（價差未實現 PnL + Funding 收到 − 已付開倉手續費），並標示尚未包含平倉成本。
`SIMULATION` 產生的配對 SHALL 顯示「模擬」而不是 Funding 數值。

#### Scenario: 有流水的腿

- **WHEN** Binance 腿累計歸屬流水為 −0.12、Bybit 腿為 +0.36
- **THEN** Binance 腿顯示 −0.12 USDT（負向色）、Bybit 腿顯示 +0.36 USDT（正向色），配對顯示 +0.24 USDT

#### Scenario: 尚未取得不顯示為零

- **WHEN** 某腿的流水尚未取得
- **THEN** Funding 收到顯示「—」與「尚未取得」，而不是 0.00

#### Scenario: 進行中合計標示不含平倉成本

- **WHEN** 檢視一組持倉中的配對卡
- **THEN** 「進行中合計」旁標示「尚未包含平倉成本」

### Requirement: 結算時間軸

系統 SHALL 為每組配對提供結算時間軸，依時間遞增列出持倉區間內的每一個結算時段：結算時間（UTC）、交易所與腿、金額、該時段狀態、累計 Funding。
結算時段 SHALL 來自 `funding-observation` 推算的預期結算時間，並與已取得的流水逐一對應；狀態 SHALL 為「已收到」（有流水）、「待結算」（時間尚未到或尚在取得延遲內）、「缺少」（已過取得重試窗仍無流水）。
時間軸 SHALL 同時列出兩腿各自的結算（週期不同時兩腿的結算時段不同），SHALL NOT 假設兩腿同時結算。
「缺少」的時段 SHALL 以警示色標示並說明該配對的 PnL 為 `INCOMPLETE`。

#### Scenario: 兩腿週期不同

- **WHEN** Binance 腿週期為 4 小時、Bybit 腿週期為 8 小時，持倉跨過 Binance 的一次結算
- **THEN** 時間軸只列出 Binance 腿的該次結算，Bybit 腿沒有時段

#### Scenario: 缺少流水

- **WHEN** 某次預期結算已過重試窗但沒有對應流水
- **THEN** 該時段狀態為「缺少」，以警示色標示

#### Scenario: 累計值依序累加

- **WHEN** 時間軸有兩個「已收到」時段，金額依序為 +0.36 與 −0.12
- **THEN** 累計 Funding 依序顯示 +0.36 與 +0.24

### Requirement: 預期對實際比較面板

配對詳情 SHALL 提供「預期對實際」面板，逐項顯示預期與實際的 Funding、手續費、滑價、Net 與差異，以及 PnL 拆解（Funding PnL、價差 PnL、開倉與平倉手續費、滑價、其他成本、Net PnL）。
「其他成本」為 0 且無資料來源時 SHALL 標示「未納入」；`INCOMPLETE` 的結果 SHALL 在面板頂部顯示狀態與原因清單，缺少的項目顯示「—」。
沒有預期快照時，預期欄 SHALL 顯示「無預期快照」。
面板 SHALL 標示該配對執行時的模式，且 SHALL NOT 對 `SIMULATION` 配對顯示實際欄。

#### Scenario: 完整結果

- **WHEN** 配對 PnL 為 `COMPLETE`
- **THEN** 面板顯示各分量與 Net PnL，且 Net PnL 等於各分量之和

#### Scenario: 不完整結果

- **WHEN** 配對 PnL 為 `INCOMPLETE`，原因為「缺少結算流水」
- **THEN** 面板頂部顯示 `INCOMPLETE` 與該原因，Funding 欄顯示「—」

#### Scenario: 其他成本未納入

- **WHEN** 其他成本沒有資料來源
- **THEN** 該項顯示 0.00 並標示「未納入」

### Requirement: 對帳差異與資料問題的警示呈現

`PNL_RECONCILIATION` 的 `MISMATCH`、`FUNDING_LEDGER_CONFLICT`、`FETCH_ERROR`（funding 取得失敗）與 `INCOMPLETE` 的 PnL SHALL 在頁面上以警示呈現，並 SHALL 連結到對應事件，使使用者能在系統日誌檢視原始內容。
警示 SHALL 沿用 `alert-banner` 的機制，SHALL NOT 自動修正任何資料。
警示 SHALL 持續到使用者可見的狀態改變（例如重算後為 `COMPLETE` 且對帳為 `OK`）。

#### Scenario: 對帳差異警示

- **WHEN** 某配對寫入 `MISMATCH` 的對帳事件
- **THEN** 頁面顯示警示並提供該事件的連結

#### Scenario: 重算後警示解除

- **WHEN** 重算後 PnL 為 `COMPLETE` 且最新對帳為 `OK`
- **THEN** 該配對的警示不再顯示
