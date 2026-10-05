## ADDED Requirements

### Requirement: 掃幣表的列集合與欄位

掃幣頁 SHALL 以 Funding Rate Matrix 列出標的，每個標的一列。列集合為：在已啟用的交易所（`allowed_exchanges`）中至少一所有 `LISTED` 觀測、且（`allowed_coins` 非空時）在 `allowed_coins` 內的標的。
欄位 SHALL 為：Rank、Symbol、覆蓋、結算倒數、Binance、Bybit、OKX、最佳套利方向、Gross Spread %、Net Edge %、達標。
表格 SHALL NOT 包含 Pionex、Bitget 或任何系統未接入的交易所欄位。
「覆蓋」SHALL 顯示「有 `LISTED` 觀測的交易所數 / 已啟用的交易所數」（例如 2/3）。
表格 SHALL 只繪製可見的列（虛擬化），不得為全部列各建一個畫面元件。

#### Scenario: 沒有佔位欄

- **WHEN** 開啟掃幣頁
- **THEN** 欄位中沒有 Pionex 與 Bitget

#### Scenario: 覆蓋的顯示

- **WHEN** 三所皆啟用，某標的只有 Binance 與 Bybit 為 `LISTED`
- **THEN** 覆蓋顯示 2/3

#### Scenario: 不在允許幣種內的標的

- **WHEN** `allowed_coins` 為 BTC 與 ETH
- **THEN** 表格只有 BTCUSDT 與 ETHUSDT 兩列

### Requirement: 每個交易所的 rate 儲存格附 funding 週期標籤

每個交易所的 rate 儲存格 SHALL 顯示 funding rate（以百分比數值、小數 4 位）與其週期標籤（例如「4h」「8h」「1h」），標籤取自 `funding_interval_secs`。
週期不是 8 小時時，儲存格 SHALL 另以次要文字顯示 8h 等效值（標示「僅供顯示」）；8h 等效值 SHALL NOT 用於 Net Edge 與達標。
`data_status` 為 `DATA_ERROR` 的觀測 SHALL 顯示「資料異常」與標籤「週期未知」（若是週期問題），其 rate SHALL NOT 參與 Gross Spread、最佳方向、Net Edge 與達標。
`NOT_LISTED` 或該所沒有資料的儲存格 SHALL 顯示「—」。
rate 的顏色 SHALL 使用 `design-tokens` 的 funding rate 顏色語意（正、負、零）。
OKX 的儲存格 SHALL 顯示公開行情的 rate 與週期標籤，並帶有「僅比價」標示。

#### Scenario: 4 小時週期的標籤與 8h 等效

- **WHEN** Binance 某標的 rate 為 0.0005（小數）、週期 14400 秒
- **THEN** 儲存格顯示「0.0500」與「4h」，次要文字顯示 8h 等效 0.1000（僅供顯示）

#### Scenario: 週期查不到

- **WHEN** 某所某標的 `data_status` 為 `DATA_ERROR`
- **THEN** 該儲存格顯示「資料異常」，且該所不被納入該列的配對計算

#### Scenario: OKX 欄位標示

- **WHEN** 某標的 OKX 有資料
- **THEN** OKX 儲存格顯示 rate、週期標籤與「僅比價」

### Requirement: Gross Spread 與 Net Edge 兩欄

掃幣表 SHALL 同時顯示 Gross Spread % 與 Net Edge %。
Gross Spread SHALL 為該列最佳配對兩腿 rate 差的絕對值（百分比、小數 4 位），僅供顯示；兩腿週期不同時，SHALL 另顯示「週期不同」標示，提醒 Gross Spread 不等於一次結算的收益。
Net Edge SHALL 由 `core` 的 Net Edge 函式計算（單次結算模型），並以該配對生效的設定（`core` 的每腿保守值合併）為輸入；頁面 SHALL NOT 自行實作 Net Edge 公式。
任何 Net Edge 必填設定缺失（`net_edge_threshold_pct`、`est_slippage_pct`、任一腿交易所的 `taker_fee_pct`）時，Net Edge 欄 SHALL 顯示「未設定」並列出缺少的項目，SHALL NOT 顯示 0。
配對的任一腿不是 `LISTED` 時，Net Edge 欄 SHALL 顯示「—」。
Net Edge 為負時 SHALL 使用負向色。

#### Scenario: Gross Spread 與 Net Edge 並列

- **WHEN** Binance rate 0.0003、Bybit rate −0.0001，兩腿皆在同一時間結算，各所 `taker_fee_pct` 為 0.02，`est_slippage_pct` 與 `safety_margin_pct` 為 0
- **THEN** Gross Spread 顯示 0.0400，Net Edge 顯示 −0.0400，且為負向色

#### Scenario: 費率未設定

- **WHEN** Bybit 的 `taker_fee_pct` 沒有設定
- **THEN** 所有涉及 Bybit 的列 Net Edge 欄顯示「未設定」，並指出缺少 Bybit 的 `taker_fee_pct`，而不是 0

#### Scenario: 週期不同的列

- **WHEN** 某列兩腿的週期分別為 4h 與 8h
- **THEN** Gross Spread 旁顯示「週期不同」

### Requirement: 達標判定、達標欄與預設排序

「達標」SHALL 完全由 `core` 的達標判定決定（Net Edge ≥ `net_edge_threshold_pct`、兩腿 `LISTED`、兩腿成交量不小於 `min_24h_volume_usdt`、交易所與幣種在允許清單內），頁面 SHALL NOT 另加或略過條件。
達標欄 SHALL 顯示「達標」或「—」；設定不完整時 SHALL 顯示「未設定」，SHALL NOT 顯示為未達標。
預設排序 SHALL 為：Net Edge 可得時依 Net Edge 由高到低（相同時依 Gross Spread 由高到低），沒有 Net Edge 的列排在最後；設定不完整時依 Gross Spread 由高到低。Rank SHALL 依此排序編號。
「最佳套利方向」SHALL 顯示 long 與 short 的交易所（long 為 rate 較低者）；Net Edge 可得時取 Net Edge 最高的配對，不可得時取 Gross Spread 最大的配對。

#### Scenario: 達標

- **WHEN** 某列 Net Edge 為 0.0200、`net_edge_threshold_pct` 為 0.01，其他條件皆成立
- **THEN** 達標欄顯示「達標」

#### Scenario: Gross Spread 大但 Net Edge 不足

- **WHEN** Gross Spread 為 0.2000、Net Edge 為 0.0050、門檻為 0.01
- **THEN** 達標欄顯示「—」

#### Scenario: 預設排序

- **WHEN** 三列的 Net Edge 為 0.01、0.03、−0.02
- **THEN** Rank 1 至 3 依序為 0.03、0.01、−0.02

#### Scenario: 設定不完整時的達標欄

- **WHEN** `net_edge_threshold_pct` 沒有設定
- **THEN** 所有列的達標欄顯示「未設定」，排序改依 Gross Spread

### Requirement: OKX 僅供比價，不參與達標與方向

OKX 目前沒有下單能力，因此達標判定與「最佳套利方向」SHALL 只在具備下單能力的交易所之間計算（由單一常數集合定義，目前為 Binance 與 Bybit）；OKX 的 rate 只顯示、不參與 Gross Spread、Net Edge、最佳方向與達標。
標的在可下單的交易所中少於兩所為 `LISTED` 時，Gross Spread、Net Edge、最佳方向與達標欄 SHALL 顯示「—」。
「覆蓋」欄 SHALL 仍計入 OKX。

#### Scenario: OKX 的價差較大也不被選為方向

- **WHEN** 某標的 OKX 與 Binance 的 rate 差最大，但 Binance 與 Bybit 之間也有可計算的配對
- **THEN** 最佳套利方向只在 Binance 與 Bybit 之間決定，OKX 儲存格仍顯示其 rate 與「僅比價」

#### Scenario: 只有一個可下單的交易所

- **WHEN** 某標的只有 Binance 與 OKX 為 `LISTED`
- **THEN** Gross Spread、Net Edge、最佳方向、達標皆顯示「—」，覆蓋顯示 2/3

### Requirement: 各標的獨立的結算倒數

每一列 SHALL 顯示自己的「結算倒數」（HH:MM:SS），其目標時間 SHALL 為該列最佳配對兩腿 `next_funding_time` 的較早者（沒有配對時，為該列所有 `LISTED` 觀測中最早的 `next_funding_time`）。
倒數 SHALL 以注入的時鐘加上該目標時間所屬交易所的 serverTime 偏移量計算，並每秒更新；各列倒數 SHALL 互相獨立（4 小時週期與 8 小時週期的標的可以不同）。
倒數歸零或為負時，該列 SHALL 顯示「結算中」，並 SHALL NOT 顯示負數；該列的資料 SHALL 在下一次更新取得新的結算時間之前被標示為「待更新」。
頁面 SHALL NOT 以單一全域倒數代表所有標的。

#### Scenario: 兩列的倒數不同

- **WHEN** 現在為 04:00:00（UTC），某 4 小時週期標的下次結算為 08:00:00，某 8 小時週期標的下次結算為 16:00:00
- **THEN** 兩列的倒數分別顯示 04:00:00 與 12:00:00

#### Scenario: 週期不同的配對取較早者

- **WHEN** 某配對 long 腿下次結算 08:00、short 腿 04:00，現在為 03:00
- **THEN** 倒數目標為 04:00，顯示 01:00:00

#### Scenario: 使用校時後的時間

- **WHEN** 本機時鐘比交易所快 2000 毫秒（偏移量 −2000），距目標時間的未校正剩餘為 60 秒
- **THEN** 顯示的倒數為 62 秒

#### Scenario: 超過結算時間

- **WHEN** 倒數已過 0 而新資料尚未到達
- **THEN** 該列顯示「結算中」與「待更新」，不顯示負數

### Requirement: 只顯示達標 toggle 與符合筆數

頁面 SHALL 提供明確的「只顯示達標」toggle（有開關的視覺狀態與標籤），旁邊 SHALL 顯示「符合 N 筆」，N 為達標列數（不論 toggle 開或關都是同一個數）。
toggle 開啟時表格只顯示達標列；關閉時顯示全部列。預設為關閉。
N SHALL 與彙總卡的「達標」數字一致。設定不完整（達標無法判定）時，toggle SHALL 為停用，並顯示「設定不完整，無法判斷達標」，SHALL NOT 顯示「符合 0 筆」。
toggle 開啟且 N 為 0 時，表格 SHALL 顯示「目前沒有達標標的」，與「載入中」及「無法取得行情」的狀態明確區分。

#### Scenario: 開啟 toggle

- **WHEN** 共有 20 列、其中 12 列達標，使用者開啟 toggle
- **THEN** 表格顯示 12 列，旁註「符合 12 筆」，彙總卡的達標數字也是 12

#### Scenario: 關閉 toggle

- **WHEN** 使用者關閉 toggle
- **THEN** 表格顯示全部 20 列，旁註仍是「符合 12 筆」

#### Scenario: 設定不完整時停用

- **WHEN** 風控設定不完整
- **THEN** toggle 為停用狀態並顯示「設定不完整，無法判斷達標」，不顯示「符合 0 筆」

#### Scenario: 開啟後沒有達標標的

- **WHEN** 設定完整、toggle 開啟、沒有任何列達標
- **THEN** 表格顯示「目前沒有達標標的」

### Requirement: 立即刷新必須重新抓取並重算

「立即刷新」SHALL 對每一個已啟用的資料來源發出新的請求（繞過輪詢快取，見 `exchange-adapter` 的重新抓取規則），待回應後以新的觀測重新計算整張表，SHALL NOT 只重繪畫面或重新排序既有資料。
刷新期間按鈕 SHALL 顯示進行中並停用，重複點擊 SHALL 被忽略，避免並行的重複請求。
刷新完成後「最新掃描」時間 SHALL 更新為本次重算的時間（UTC）。
部分來源失敗時，成功的來源 SHALL 照常更新，失敗來源的欄位 SHALL 標示過期與錯誤，SHALL NOT 把它的舊值當作新資料呈現。
所有來源皆失敗時，頁面 SHALL 保留舊表格並標示「無法取得行情」與各來源的錯誤。

#### Scenario: 刷新發出新請求

- **WHEN** 以假傳輸層記錄請求，使用者點擊「立即刷新」
- **THEN** 每個已啟用的來源各記錄到新的請求，且重算後各觀測的 `observed_at` 晚於刷新前

#### Scenario: 刷新期間重複點擊

- **WHEN** 刷新尚未完成時使用者再次點擊
- **THEN** 第二次點擊被忽略，傳輸層沒有額外的請求

#### Scenario: 一個來源失敗

- **WHEN** 刷新時 Bybit 逾時、Binance 與 OKX 成功
- **THEN** Binance 與 OKX 的欄位更新，Bybit 的欄位標示過期與逾時錯誤，「最新掃描」時間仍更新

#### Scenario: 所有來源失敗

- **WHEN** 刷新時三個來源都失敗
- **THEN** 舊表格保留，並顯示「無法取得行情」與各來源的錯誤，且舊資料帶有年齡標示

### Requirement: 頁首的門檻、彙總卡與來源狀態

頁首 SHALL 以唯讀方式顯示目前生效的 `net_edge_threshold_pct`（標示為「Net Edge 門檻 %」），未設定時顯示「未設定」；修改門檻 SHALL 在風控設定頁進行，頁首 SHALL 提供前往的連結。
彙總卡 SHALL 顯示：掃描標的數（列集合筆數）、多所覆蓋數（覆蓋至少兩所 `LISTED` 的標的數）、達標數（不可判定時顯示「未設定」）。
頁首 SHALL 顯示每個資料來源的狀態與新鮮度：Binance WebSocket 的連線狀態（ONLINE、RECONNECTING、OFFLINE）、Bybit 與 OKX 的下次輪詢倒數；以及「最新掃描」時間。
頁面 SHALL NOT 顯示「Demo 預檢通過」之類的字樣；預檢只在送單當下進行。

#### Scenario: 門檻未設定

- **WHEN** `net_edge_threshold_pct` 沒有設定
- **THEN** 頁首顯示「Net Edge 門檻 %：未設定」與前往風控設定的連結

#### Scenario: 彙總卡的數字與表格一致

- **WHEN** 表格有 248 列、其中 20 列覆蓋至少兩所、12 列達標
- **THEN** 彙總卡依序顯示 248、20、12

#### Scenario: WebSocket 斷線

- **WHEN** Binance WebSocket 已斷線
- **THEN** 頁首的 Binance 狀態顯示 OFFLINE，不顯示 ONLINE

### Requirement: 表格重算有頻率上限

由行情更新觸發的表格重算 SHALL 有頻率上限（預設每秒 2 次，由 design.md 決定），上限之間的多次更新 SHALL 合併為一次；結算倒數每秒更新一次且 SHALL 不觸發完整重算。
528 列的表格在實際更新頻率下的幀時間 SHALL 滿足 `bootstrap-gpui-shell` 記錄的預算（p95 ≤ 16.7 ms，若該 change 有放寬則以放寬後為準）。

#### Scenario: 一秒內的多次更新被合併

- **WHEN** 1 秒內 WebSocket 推送了 5 次更新
- **THEN** 表格在該秒內最多重算 2 次，最終顯示的資料包含全部 5 次更新

#### Scenario: 倒數不觸發重算

- **WHEN** 只有時間經過 1 秒、沒有新資料
- **THEN** 只有倒數儲存格更新，Net Edge 與達標沒有被重新計算
