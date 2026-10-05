## ADDED Requirements

### Requirement: 配對 PnL 拆解且 Net PnL 恆等於各分量之和

系統 SHALL 對每一組配對（及其每一腿）計算下列分量，單位 USDT，使用 Decimal，SHALL NOT 在 core 內做顯示用的四捨五入：

- Funding PnL：該腿的 funding 流水合計（收到為正、支付為負）。
- 價差 PnL（參考價）：long 腿為 (平倉參考價 − 開倉參考價) × 數量；short 腿為 (開倉參考價 − 平倉參考價) × 數量。參考價為送單當下記錄的預期價格（見滑價需求）。
- 開倉手續費、平倉手續費：取自交易所回報的成交明細，以正數表示支出（負值代表返佣）。
- 滑價：價差 PnL（參考價）減去以實際成交價計算的價差 PnL；正值為不利於本方的成本。
- 其他成本：獨立分量；沒有任何資料來源時 SHALL 為 0 並標示「未納入」，SHALL NOT 省略此分量。

Net PnL SHALL 等於 Funding PnL + 價差 PnL（參考價）− 開倉手續費 − 平倉手續費 − 滑價 − 其他成本。
由於滑價已自參考價 PnL 扣除，等價的實際基礎算式 Funding PnL + 價差 PnL（實際成交價）− 開倉手續費 − 平倉手續費 − 其他成本 SHALL 得到相同結果，系統 SHALL NOT 對同一筆價格差異扣兩次。
配對總計 SHALL 等於兩腿各分量之和。只有一腿有成交的配對（例如 `PARTIAL_FAILURE` 後人工平倉）SHALL 以現有成交計算並照常拆解。

#### Scenario: 手算的完整案例

- **WHEN** 配對的 Funding PnL 為 +0.24、價差 PnL（參考價）為 0、開倉手續費 0.48、平倉手續費 0.48、滑價 0.10、其他成本 0
- **THEN** Net PnL 為 −0.82 USDT（0.24 + 0 − 0.48 − 0.48 − 0.10 − 0）

#### Scenario: 兩種算式一致

- **WHEN** 同一案例以實際成交價計得價差 PnL 為 −0.10
- **THEN** 以實際基礎算式 0.24 + (−0.10) − 0.48 − 0.48 − 0 同樣得到 −0.82 USDT

#### Scenario: Short 腿價差方向

- **WHEN** short 腿開倉參考價 60,000、平倉參考價 60,200、數量 0.02
- **THEN** 該腿價差 PnL（參考價）為 −4.00 USDT

#### Scenario: 手續費幣別不是 USDT

- **WHEN** 某筆成交的手續費以 USDT 以外的幣別計價且沒有可驗證的換算價
- **THEN** 該手續費分量標為無法換算，整體 PnL 狀態為 `INCOMPLETE`，而不是以 0 或猜測匯率代入

### Requirement: 成交比與滑價分析

系統 SHALL 對每一腿的開倉與平倉分別記錄 Target Notional、Requested Notional、Actual Filled Notional，並計算 FillRatio = Actual Filled Notional ÷ Requested Notional。
系統 SHALL 對每一次成交記錄 Expected Price（送單當下的參考價）、Actual Fill Price、價差與 Slippage %：long 為 (Actual − Expected) ÷ Expected，short 為 (Expected − Actual) ÷ Expected，使正值恆代表不利（成本）；並 SHALL 彙總 Long Slippage、Short Slippage 與 Total Slippage。
Expected Price 不存在時，該次成交的滑價 SHALL 標為「無參考價」並使 PnL 狀態為 `INCOMPLETE`，SHALL NOT 以實際成交價代替參考價而使滑價恆為 0。

#### Scenario: 成交比

- **WHEN** Requested Notional 為 1,000、Actual Filled Notional 為 997.42
- **THEN** FillRatio 為 0.99742

#### Scenario: Long 滑價為不利

- **WHEN** long 腿 Expected Price 為 60,000、Actual Fill Price 為 60,006
- **THEN** Slippage % 為 +0.01%

#### Scenario: Short 滑價為不利

- **WHEN** short 腿 Expected Price 為 60,000、Actual Fill Price 為 59,994
- **THEN** Slippage % 為 +0.01%（賣得較低屬不利）

#### Scenario: 缺少參考價

- **WHEN** 某次成交沒有記錄 Expected Price
- **THEN** 該次滑價顯示「無參考價」，PnL 狀態為 `INCOMPLETE`

### Requirement: 流水歸屬至配對的規則

某筆 funding 流水 SHALL 歸屬至配對的某一腿，當且僅當：交易所與該腿相同、標的與該腿相同，且結算時間大於該腿開倉最後一筆成交的時間並小於或等於該腿平倉最後一筆成交的時間（尚未平倉時以目前時間為止）。
同一交易所同一標的在該區間內存在另一個配對時 SHALL NOT 自動歸屬，並 SHALL 標示為歸屬不明、使 PnL 狀態為 `INCOMPLETE`（正常情況下 `pretrade-validation` 的 `ExistingExposure` 使此情形不會出現）。
不屬於任何配對的 funding 流水 SHALL 仍保存於事件表，並標為「未歸屬」，SHALL NOT 被丟棄。

#### Scenario: 結算在開倉成交之前

- **WHEN** 某筆流水的結算時間早於該腿開倉最後成交時間
- **THEN** 該筆不歸屬於此配對

#### Scenario: 結算恰在平倉成交時間

- **WHEN** 某筆流水的結算時間等於該腿平倉最後成交時間
- **THEN** 該筆歸屬於此配對

#### Scenario: 未歸屬流水保留

- **WHEN** 取得一筆標的與時間皆不屬於任何配對的 funding 流水
- **THEN** 事件表保留該筆並標為「未歸屬」

### Requirement: PnL 狀態與不完整判定

每一份 PnL 結果 SHALL 帶有狀態：`COMPLETE` 或 `INCOMPLETE`，後者 SHALL 附上原因清單。
下列任一情況 SHALL 使狀態為 `INCOMPLETE`：預期的結算次數多於已取得的流水筆數（預期結算由 `funding-observation` 的結算時間與持倉區間推算）；funding 取得失敗或超出保留範圍；成交明細缺漏；手續費無法換算；缺少參考價；流水歸屬不明。
缺少的資料 SHALL 以「—」或具名原因呈現，SHALL NOT 以 0 代入而顯示成 `COMPLETE`。
`SIMULATION` 的配對 SHALL NOT 產生 PnL（沒有真實成交與流水），其畫面 SHALL 標示「模擬，無實際 PnL」，SHALL NOT 將模擬結果標示為實際。

#### Scenario: 缺少一次結算的流水

- **WHEN** 配對持倉期間跨過一次結算，但重試窗結束後仍沒有對應的流水
- **THEN** PnL 狀態為 `INCOMPLETE`，原因包含「缺少結算流水」，Funding PnL 不被當作 0 的完整值

#### Scenario: 全部資料齊全

- **WHEN** 預期結算次數等於已取得流水筆數，成交明細、手續費換算與參考價皆齊全
- **THEN** PnL 狀態為 `COMPLETE`

#### Scenario: SIMULATION 配對

- **WHEN** 配對由 `SIMULATION` 產生
- **THEN** 不產生 PnL，畫面標示「模擬，無實際 PnL」

### Requirement: 與交易所流水對帳且差異必須警示

對帳 SHALL 對配對的每一腿，重新向交易所取得該配對時間窗內的 funding 流水（不使用本地快取），與事件表中已歸屬該腿的合計逐腿比較。
比較 SHALL 使用 Decimal 精確比較（兩者皆源自交易所的同一資料，不容許容差），並同時比較筆數。
每次對帳 SHALL 寫入 `PNL_RECONCILIATION` 事件，結果為 `OK` 或 `MISMATCH`，MISMATCH 事件 SHALL 含交易所、標的、時間窗、本地合計、重新取得的合計與差異；MISMATCH SHALL 觸發警示並使 PnL 狀態為 `INCOMPLETE`，SHALL NOT 自動修改或覆寫任何已記錄的流水。
重新取得失敗時 SHALL 記為對帳失敗（不是 OK）。

#### Scenario: 一致

- **WHEN** 重新取得的流水合計與筆數和本地完全相同
- **THEN** 寫入結果為 `OK` 的 `PNL_RECONCILIATION` 事件

#### Scenario: 金額不一致

- **WHEN** 本地 Binance 腿合計為 −0.12，重新取得為 −0.15
- **THEN** 寫入 `MISMATCH` 事件記錄 −0.12、−0.15 與差異 −0.03，並觸發警示

#### Scenario: 交易所多出一筆

- **WHEN** 重新取得的流水比本地多一筆
- **THEN** 結果為 `MISMATCH`，事件註明筆數差異

#### Scenario: 重新取得失敗

- **WHEN** 對帳時重新向交易所取得流水失敗
- **THEN** 記為對帳失敗並警示，不記為 `OK`

### Requirement: 預期對實際的逐項比較

系統 SHALL 在送單當下保存該配對的預期值快照：預期 funding 收入、預期手續費、預期滑價成本、安全邊際、`net_edge_usdt`（皆來自 `net-edge` 計算）。
PnL 計算後 SHALL 逐項比較預期與實際（Funding、手續費、滑價、Net），輸出差異金額與差異百分比；安全邊際沒有實際對應項，SHALL 只作為預期端顯示。
預期值基於單次結算與四筆成交的假設；實際結算次數與假設不同時，比較結果 SHALL 標示此差異。
沒有預期快照時 SHALL 顯示「無預期快照」，SHALL NOT 以事後重算的 Net Edge 代替。

#### Scenario: 逐項差異

- **WHEN** 預期 funding 收入為 0.40、實際 Funding PnL 為 0.24
- **THEN** 比較顯示 Funding 差異 −0.16 與 −40%

#### Scenario: 實際結算次數多於假設

- **WHEN** 配對持倉期間實際經歷 2 次結算，預期假設為 1 次
- **THEN** 比較結果標示「實際結算次數 2，預期假設 1」

#### Scenario: 沒有預期快照

- **WHEN** 某配對缺少送單當下的預期值快照
- **THEN** 該配對的預期欄顯示「無預期快照」

### Requirement: PnL 結果以不可變事件保存

系統 SHALL 在配對確認兩腿平倉後計算 PnL 並寫入 `PAIR_PNL_COMPUTED` 事件，內容為完整分量、狀態、原因清單、使用的流水去重鍵與成交識別。
延後到達的資料使結果改變時，系統 SHALL 寫入新的 `PAIR_PNL_RECOMPUTED` 事件，SHALL NOT 修改既有事件；任一時刻的有效結果 SHALL 為該配對最新的一筆 PnL 事件。
取得 funding 流水 SHALL 重試直到所有預期結算皆已取得或重試窗結束；重試窗結束時以當下資料計算並標為 `INCOMPLETE`。

#### Scenario: 重算不改寫舊事件

- **WHEN** 配對先以 `INCOMPLETE` 計算，之後缺漏的流水到達
- **THEN** 寫入新的 `PAIR_PNL_RECOMPUTED` 事件，原 `PAIR_PNL_COMPUTED` 事件不變，有效結果為新事件

#### Scenario: 重試窗結束

- **WHEN** 重試窗結束時仍缺少一次結算的流水
- **THEN** 以現有資料計算並寫入狀態為 `INCOMPLETE` 的 `PAIR_PNL_COMPUTED`
