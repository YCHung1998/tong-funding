## ADDED Requirements

### Requirement: 取得 OKX 資金費流水並標準化

系統 SHALL 以 `GET /api/v5/account/bills-archive` 取得 OKX 資金費流水，參數含 `instType=SWAP`、該標的的 `instId`、`type=8`、時間窗的 `begin` / `end`、`limit=100`；請求 SHALL 遵守 `okx-signed-read` 的模擬交易邊界。
只有 `subType` 為 `173`（資金費支出）或 `174`（資金費收入）的列 SHALL 成為流水項目：金額為 `balChg`（帶號，收入為正），幣別為 `ccy`，時間為 `ts`，交易所端 id 為 `billId`，標的由 `instId` 轉為 `BASEUSDT`。
`173` 的 `balChg` 大於 0、`174` 的 `balChg` 小於 0、或 `ccy` 不是 `USDT` 時，該頁 SHALL 視為解析失敗而不寫入任何項目。
本 requirement 取代 `funding-history-fetch` 中「OKX SHALL NOT 取得流水」一句。

#### Scenario: 資金費支出

- **WHEN** 帳單列為 `subType = 173`、`balChg = "-0.42"`、`ccy = USDT`、`instId = BTC-USDT-SWAP`、`billId = 623950854533513219`
- **THEN** 產生 `BTCUSDT` 的流水項目，金額 −0.42 USDT，去重鍵為 `okx:` 開頭並含該 `billId`

#### Scenario: 正負號與子類型矛盾

- **WHEN** 某列 `subType = 174` 但 `balChg = "-0.10"`
- **THEN** 該頁解析失敗，不寫入任何項目，該時間窗記為取得失敗

### Requirement: OKX 流水以 billId 分頁且無法確認完整即失敗

每個不超過 7 天的時間窗 SHALL 以上一頁最後一筆的 `billId` 作為 `after` 繼續分頁，直到某頁原始筆數少於 `limit`。
任一頁失敗、`billId` 游標重複、或達到頁數上限時，該時間窗 SHALL 記為失敗（`FETCH_ERROR`），SHALL NOT 記為「已取得、沒有項目」；失敗前已解析的項目仍 SHALL 寫入（寫入以交易所端 id 去重）。
OKX 金鑰不可用時，OKX 的抓取 SHALL 被略過並記錄原因，SHALL NOT 影響其他交易所的抓取。

#### Scenario: 兩頁取完

- **WHEN** 第一頁回傳 100 列、第二頁回傳 12 列
- **THEN** 共送出兩個請求，第二個請求的 `after` 為第一頁最後一列的 `billId`，時間窗記為已取得

#### Scenario: 游標重複

- **WHEN** 第二頁最後一列的 `billId` 與第一頁最後一列相同
- **THEN** 時間窗記為失敗，原因指出游標重複

### Requirement: OKX 腿成交以合約面值換算為幣量

engine 對 OKX 腿寫入 `ORDER_SUBMITTED` 與 `ORDER_FILL` 事件時 SHALL 一併記錄送單換算所用的 `ct_val`。
PnL 計算 SHALL 以 `filled_quantity × ct_val` 作為 OKX 腿成交的幣量，並 SHALL 使用該筆成交的 `avg_price` 與已記錄的參考價計算價格分量與滑價。
事件缺少 `ct_val` 時，該筆成交的數量與價格 SHALL 視為未知、PnL SHALL 為 INCOMPLETE 並註明原因，SHALL NOT 以 1 或查詢當下的 `ctVal` 代替。
Binance 與 Bybit 腿的計算 SHALL 不受影響。

#### Scenario: OKX 腿以幣量進入 PnL

- **WHEN** OKX 多腿開倉事件 `filled_quantity = 3`、`ct_val = 0.01`、`avg_price = 60000`，平倉 `filled_quantity = 3`、`avg_price = 60300`
- **THEN** OKX 腿的價格損益為 (60300 − 60000) × 0.03 = 9 USDT

#### Scenario: 舊事件缺合約面值

- **WHEN** OKX 腿的成交事件沒有 `ct_val`
- **THEN** PnL 為 INCOMPLETE，原因含「OKX 成交缺合約面值」，價格分量不被計算

### Requirement: OKX 腿納入 funding 歸屬與對帳

OKX 腿的 funding 取得狀態 SHALL 依其時間窗的抓取結果判定（已取得 / 失敗 / 未取得），SHALL NOT 固定為未取得。
對帳 SHALL 對 OKX 腿重新抓取其持倉期間的流水並與已歸屬的項目逐筆比對；OKX 抓取失敗時該腿 SHALL 記為 FAILED 並註明原因。

#### Scenario: OKX 腿對帳一致

- **WHEN** 一個 Bybit + OKX 的配對已平倉，OKX 腿持倉期間有兩筆資金費，重新抓取得到相同兩筆
- **THEN** `PNL_RECONCILIATION` 中 OKX 腿為 `OK`

#### Scenario: OKX 金鑰不可用

- **WHEN** 對帳時 OKX 沒有 passphrase
- **THEN** 該配對的對帳被略過並記錄原因（與 Binance / Bybit 金鑰不可用時相同）
