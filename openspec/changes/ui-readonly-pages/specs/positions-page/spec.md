## ADDED Requirements

### Requirement: 持倉頁的標題計數與篩選器

持倉頁標題 SHALL 顯示「N OPEN · M PAIRS」（N 為全部持倉列數，M 為配成組的組數）。
頁面 SHALL 提供「Exchange」與「Coin」兩個多選篩選器，選項取自目前實際存在持倉的交易所與幣種，預設全選；
篩選器旁 SHALL 顯示「已選 X 個交易所 / Y 個幣種 · 顯示 A / B」，其中 B 為全部持倉列數、A 為篩選後列數。
篩選 SHALL 只影響顯示，SHALL NOT 影響配對（配對一律以全部持倉計算），也 SHALL NOT 影響標題的 N 與 M。
取消全部選項時，頁面 SHALL 顯示「未選擇任何篩選條件」的空狀態，而不是顯示全部。

#### Scenario: 只選一個交易所

- **WHEN** 共有 4 列持倉（Binance 2 列、Bybit 2 列）、2 組配對，使用者只勾選 Binance
- **THEN** 表格顯示 2 列，旁註為「已選 1 個交易所 / 2 個幣種 · 顯示 2 / 4」，標題仍為「4 OPEN · 2 PAIRS」

#### Scenario: 篩選不拆散配對

- **WHEN** 使用者只勾選 Binance
- **THEN** 配對卡片仍顯示兩腿（Binance 與 Bybit）的完整資訊

#### Scenario: 全部取消

- **WHEN** 使用者取消所有交易所
- **THEN** 顯示「未選擇任何篩選條件」，不顯示任何持倉列

### Requirement: 持倉頁的彙總卡

持倉頁 SHALL 顯示三張彙總卡：Open Positions（持倉列數，註明「與總覽一致」與「X 對 / Y 腿」）、Entry Notional（雙腿合計，並依標的列出明細）、Unrealized PnL（合計，並依交易所列出明細）。
Open Positions 的數字 SHALL 與總覽頁取自同一份資料，兩頁 SHALL 顯示相同數值。
Entry Notional SHALL 為各持倉的「進場均價 × 數量」之和；Unrealized PnL SHALL 為交易所回報的各持倉未實現損益之和，SHALL NOT 由本頁重新計算。

#### Scenario: 名目本金明細

- **WHEN** BTC 兩腿進場名目各 1,200（共 2,400）、ETH 兩腿各 1,500（共 3,000）
- **THEN** Entry Notional 顯示 5,400.00，明細為「BTC 2,400 + ETH 3,000 USDT」

#### Scenario: 與總覽一致

- **WHEN** 總覽顯示 Open Positions 為 4
- **THEN** 持倉頁的 Open Positions 也是 4

### Requirement: 持倉表的欄位與數值呈現

持倉表 SHALL 有欄位：Exchange、Symbol、Side（LONG 或 SHORT）、Size、Entry Price、Mark Price、Leverage、Unrealized PnL、Funding 收到。
Size SHALL 以標的幣數量顯示並附上幣別（例如「0.020000 BTC」），預設顯示 6 位小數；若非零數量在 6 位小數下會顯示為 0，SHALL 改以完整精度顯示。
Unrealized PnL SHALL 帶正負號與單位（例如「+4.00 USDT」），正值使用正向色、負值使用負向色。
「Funding 收到」欄 SHALL 存在，在 `funding-pnl` 提供資料之前，所有列 SHALL 顯示「—」，SHALL NOT 顯示 0.00。
表格下方 SHALL 註明「PnL 未扣手續費與資金費」，直到 `funding-pnl` 提供扣除後的數值為止。

#### Scenario: 一列持倉的呈現

- **WHEN** Binance BTCUSDT 多單，數量 0.02、進場均價 60,000、mark 60,200、槓桿 3、未實現損益 +4
- **THEN** 該列顯示「Binance · BTCUSDT · LONG · 0.020000 BTC · 60,000.00 · 60,200.00 · 3× · +4.00 USDT · —」

#### Scenario: 極小數量不顯示為零

- **WHEN** 某持倉數量為 0.0000004
- **THEN** Size 以完整精度顯示 0.0000004，而不是 0.000000

#### Scenario: Funding 欄尚無資料

- **WHEN** 開啟持倉頁
- **THEN** 「Funding 收到」欄每一列為「—」

### Requirement: 對沖配對卡片

持倉頁 SHALL 對每一組由 `position-grouping` 配成的配對顯示一張卡片，內容：標的與「中性組合」、配對狀態標籤（HEDGED 或 IMBALANCED）與不平衡率、兩腿 Entry Notional 與 Margin、LONG 與 SHORT 各自的交易所與數量、Pair Unrealized PnL。
不平衡率 SHALL 為 `|long 數量 − short 數量| ÷ max(long 數量, short 數量) × 100`；此公式為本頁的顯示定義，標籤為 IMBALANCED 的條件 SHALL 為不平衡率大於該配對生效的 `max_leg_imbalance_pct`，否則為 HEDGED。
標籤 SHALL 僅為顯示，SHALL NOT 改變配對的狀態（狀態只由 `pair-lifecycle` 決定）。
配對本身的狀態為 `PARTIAL_FAILURE`、`IMBALANCED` 或 `UNRESOLVED` 時，卡片 SHALL 以警示色顯示該狀態名稱與「需人工處理」。
Pair Unrealized PnL SHALL 為兩腿交易所回報值之和，並分別列出兩腿的數值。

#### Scenario: 完全平衡

- **WHEN** 配對兩腿數量皆為 0.02
- **THEN** 卡片顯示「HEDGED · 0.00% IMBALANCE」

#### Scenario: 超過容許的不平衡

- **WHEN** long 數量 0.020、short 數量 0.018，生效的 `max_leg_imbalance_pct` 為 1.0
- **THEN** 不平衡率為 10%，標籤為 IMBALANCED

#### Scenario: 配對狀態為單腿失敗

- **WHEN** 某配對的狀態為 `PARTIAL_FAILURE`
- **THEN** 其卡片以警示色顯示「PARTIAL_FAILURE · 需人工處理」

#### Scenario: 配對的兩腿 PnL

- **WHEN** long 腿未實現損益 +4、short 腿 −4
- **THEN** 卡片顯示「Pair Unrealized PnL 0.00 USDT（+4.00 / −4.00）」

### Requirement: 未配對的持倉要明確標示

沒有被配成組的持倉列 SHALL 在表格中帶有「未配對」標籤，且 SHALL NOT 顯示於任何配對卡片中。
「未配對」的列數 SHALL 與總覽頁曝險摘要的未避險單腿數量一致。

#### Scenario: 單腿持倉

- **WHEN** 有一列持倉找不到對應的配對
- **THEN** 該列顯示「未配對」標籤，總覽的曝險摘要顯示「1 個未避險單腿」

### Requirement: 持倉資料不完整或不可得時要明示

某交易所的持倉查詢為 `Incomplete`（分頁未取完）時，頁面 SHALL 顯示「<交易所> 持倉列表可能不完整」，並 SHALL NOT 以此列表判定「該所沒有其他持倉」。
某交易所為未連線時，頁面 SHALL 顯示其未連線，SHALL NOT 以「無持倉」呈現。
OKX 不提供帳戶資料，持倉頁 SHALL 註明「OKX 僅比價，不顯示持倉」。

#### Scenario: Bybit 持倉不完整

- **WHEN** Bybit 持倉查詢結果為 `Incomplete`
- **THEN** 頁面顯示「Bybit 持倉列表可能不完整」，已取得的列仍顯示

#### Scenario: 交易所未連線

- **WHEN** Binance 為未連線
- **THEN** 頁面顯示 Binance 未連線，而不是「Binance 無持倉」
