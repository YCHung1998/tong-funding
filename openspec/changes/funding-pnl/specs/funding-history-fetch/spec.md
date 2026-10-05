## ADDED Requirements

### Requirement: 從各所取得 funding 收付流水並標準化

系統 SHALL 以簽名的唯讀 GET 從 Binance 與 Bybit 取得 funding 收付流水：Binance 使用 `/fapi/v1/income` 並限定 `incomeType=FUNDING_FEE`；Bybit 使用 `/v5/account/transaction-log`，指定 `accountType=UNIFIED`、`category=linear` 並限定 `type=SETTLEMENT`。
端點網址 SHALL 為寫死的 demo / testnet，SHALL NOT 存在任何真錢端點。系統 SHALL NOT 對這些端點發出任何非 GET 的請求。
每筆流水 SHALL 標準化為：交易所、標的、金額（USDT、Decimal）、幣別、結算時間（毫秒）、交易所端 id、完整原始回應。金額正負號 SHALL 統一為「收到為正、支付為負」；Bybit 的 `funding` 欄位依官方文件正值為收到、負值為支付。
解析 SHALL 以對真實 demo 回應錄製的 fixtures 驗證，金額、時間單位與 id 欄位的解讀 SHALL NOT 僅憑文件假設。
OKX SHALL NOT 取得流水（不下單，沒有倉位）。

#### Scenario: Bybit 支付 funding

- **WHEN** Bybit 回傳一筆 `type=SETTLEMENT`、`funding=-0.003676` 的流水
- **THEN** 標準化金額為 −0.003676 USDT（支付）

#### Scenario: Bybit 收到 funding

- **WHEN** Bybit 回傳一筆 `type=SETTLEMENT`、`funding=0.5` 的流水
- **THEN** 標準化金額為 +0.5 USDT（收到）

#### Scenario: 非 funding 類型的流水被排除

- **WHEN** Bybit 回應同時含 `type=TRADE` 與 `type=SETTLEMENT`
- **THEN** 只有 `SETTLEMENT` 被轉為 funding 流水

#### Scenario: 交易所端 id 組成去重鍵

- **WHEN** Binance 回傳 `incomeType=FUNDING_FEE`、`tranId=9689322392` 的流水
- **THEN** 去重鍵為 `binance:FUNDING_FEE:9689322392`，且 Bybit 的 `id` 以 `bybit:` 為前綴組成各自的鍵

### Requirement: 以不超過 7 天的時間窗分頁取得且無法確認完整即視為失敗

取得流水 SHALL 將查詢區間切成每窗不超過 7 天的時間窗（Binance 未指定時間時預設只回傳最近 7 天，Bybit 兩個時間都指定時區間不得超過 7 天），並 SHALL 逐窗分頁直到交易所表明已耗盡（Bybit 的 `nextPageCursor` 為空）。
系統 SHALL NOT 因單頁筆數上限而靜默截斷：任何無法確認該窗已取完的情況 SHALL 視為該次取得失敗。
取得失敗 SHALL 寫入 `FETCH_ERROR` 事件並使依賴該區間的 PnL 狀態為 `INCOMPLETE`；失敗前已取得的流水 MAY 寫入，因為寫入具冪等性（見去重需求）。
Binance 只保留最近三個月的 income 資料，查詢起點早於此範圍時 SHALL 顯示「超出交易所保留範圍」而不是回傳空結果當作無流水。

#### Scenario: 10 天區間切成兩個窗

- **WHEN** 要取得 10 天區間的 Bybit 流水
- **THEN** 發出兩個各不超過 7 天的查詢，結果合併

#### Scenario: 有下一頁就繼續

- **WHEN** Bybit 回應的 `nextPageCursor` 不為空
- **THEN** 以該 cursor 再取下一頁，直到 cursor 為空

#### Scenario: 任一頁失敗

- **WHEN** 某窗第二頁請求失敗
- **THEN** 該次取得標示為不完整並寫入 `FETCH_ERROR`，不被視為「該窗沒有流水」

#### Scenario: 超出保留範圍

- **WHEN** 要查詢 Binance 四個月前的 funding 流水
- **THEN** 顯示「超出交易所保留範圍」，不回傳空清單

### Requirement: 流水寫入不可變事件表並以交易所端 id 去重

每筆標準化流水 SHALL 寫入 `events` 表，事件類型為 `FUNDING_LEDGER_ENTRY`，payload 含標準化欄位、去重鍵與完整原始回應；事件 SHALL NOT 被更新或刪除。
同一去重鍵 SHALL 至多有一筆 `FUNDING_LEDGER_ENTRY` 事件，此唯一性 SHALL 由資料庫層保證，重複寫入 SHALL 為不報錯的無動作，並回報新增筆數與略過筆數。
同一去重鍵再次出現但金額與已存者不同時，系統 SHALL NOT 覆寫，SHALL 寫入 `FUNDING_LEDGER_CONFLICT` 事件（含兩個值）並觸發警示。

#### Scenario: 重複抓取不重複寫入

- **WHEN** 對同一時間窗連續抓取兩次
- **THEN** `FUNDING_LEDGER_ENTRY` 的筆數與第一次後相同，第二次回報新增 0 筆

#### Scenario: 流水事件不可修改

- **WHEN** 對 `FUNDING_LEDGER_ENTRY` 事件執行 UPDATE 或 DELETE
- **THEN** 資料庫拒絕該操作

#### Scenario: 同 id 不同金額

- **WHEN** 已存在去重鍵為 K、金額為 −0.10 的事件，之後取得同一鍵 K 且金額為 −0.12 的流水
- **THEN** 原事件不變，新增一筆 `FUNDING_LEDGER_CONFLICT` 事件記錄兩個金額

### Requirement: 取得流水的時機、限流與安全邊界

系統 SHALL 在配對持倉期間於每次預期結算後延遲一段時間取得流水，並在配對確認平倉後再取得一次；延遲與重試間隔為設計參數（見 design.md）。
取得 SHALL 使用校正過的時間，並遵守 429 與 `Retry-After` 的退避。
取得流水是唯讀動作，kill switch SHALL NOT 阻擋；但系統處於失敗即封閉的停機狀態（無法寫入資料庫）時 SHALL NOT 取得。
`SIMULATION` 下沒有真實倉位，系統 SHALL NOT 發出取得 funding 流水的請求。
demo 金鑰不可用時，流水狀態 SHALL 顯示為「未取得」，SHALL NOT 顯示為 0。

#### Scenario: kill switch 不阻擋取得

- **WHEN** kill switch 已啟動且配對仍有持倉
- **THEN** 流水仍照常取得

#### Scenario: SIMULATION 不取得

- **WHEN** `execution_mode` 為 `SIMULATION`
- **THEN** 不發出任何 income 或 transaction-log 請求

#### Scenario: 金鑰不可用

- **WHEN** 無法從 Keychain 取得 demo 金鑰
- **THEN** funding 流水狀態為「未取得」，持倉頁的 Funding 收到顯示「—」

#### Scenario: 被限流

- **WHEN** 交易所回應 429 並帶 `Retry-After`
- **THEN** 依該值退避後才重試
