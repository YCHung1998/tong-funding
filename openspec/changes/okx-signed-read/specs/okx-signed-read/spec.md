## ADDED Requirements

### Requirement: OKX 簽名請求只能以模擬交易身分送出

OKX 的每一個簽名請求 SHALL 送往程式中寫死的 OKX 簽名主機常數，且 SHALL 帶有標頭 `x-simulated-trading: 1`；取得 OKX 簽名網址的唯一途徑 SHALL 同時給出該標頭，且每個 OKX 請求的建構 SHALL 無條件套用；呼叫端 SHALL NOT 有任何途徑省略、覆寫或改變其值。
傳輸層 SHALL 拒絕（不建立任何連線）主機為 OKX 但缺少該標頭、或其值不是 `1` 的請求。
主機 SHALL NOT 可由設定、環境變數或使用者輸入覆寫。由於 OKX 正式與 demo 共用主機，系統 SHALL NOT 以主機名稱作為 OKX「非正式環境」的唯一依據。
本 requirement 取代 `signed-read-access` 中「OKX SHALL NOT 有任何簽名請求的實作」一句。

#### Scenario: 每個 OKX 簽名請求都帶模擬標頭

- **WHEN** 以錄製用的傳輸層執行 OKX 的帳戶設定、餘額、持倉、委託查詢
- **THEN** 每一個被記錄的請求主機都是寫死的 OKX 簽名主機，且都帶 `x-simulated-trading: 1`

#### Scenario: 缺少模擬標頭的 OKX 請求被傳輸層拒絕

- **WHEN** 有一個主機為 OKX 簽名主機、但沒有 `x-simulated-trading: 1` 的請求被交給真實傳輸層
- **THEN** 傳輸層回傳錯誤且沒有建立任何連線

#### Scenario: 靜態檢查鎖定主機與標頭字面

- **WHEN** 執行 `exchange::static_checks`
- **THEN** OKX 簽名主機字面只出現在 `signed/endpoints.rs`，`x-simulated-trading` 字面也只出現在 `signed/endpoints.rs`，`signed/okx.rs` 不含任何主機字面

### Requirement: OKX 金鑰須含 passphrase，缺任一項即未連線且不發出請求

OKX 簽名 SHALL 需要 API key、secret 與 passphrase 三者皆存在且非空；任一缺少、為空或金鑰儲存讀取失敗時，SHALL 回報「未連線」並附原因（`NoKey`、`NoSecret`、`NoPassphrase`、`SecretStoreError`），SHALL NOT 建立或送出任何請求，SHALL NOT 以空字串簽名。
錯誤訊息、`Debug` 輸出與日誌 SHALL NOT 含 key、secret、passphrase 或簽名的任何部分。

#### Scenario: 缺少 passphrase

- **WHEN** OKX 只有 API key 與 secret
- **THEN** 查詢回報未連線（`NoPassphrase`），傳輸層沒有收到任何請求

#### Scenario: 錯誤訊息不含機密

- **WHEN** OKX 回應 `50105`（passphrase 錯誤）
- **THEN** 回報的錯誤含代碼與訊息，但不含 passphrase、key、secret 或簽名

### Requirement: OKX 簽名與時間戳

`OK-ACCESS-SIGN` SHALL 為 `Base64(HMAC-SHA256(secret, timestamp + METHOD + requestPath + body))`，其中 requestPath 含 `?` 與 query 字串、GET 的 body 為空字串；被簽名的字串 SHALL 與實際送出的路徑、query 與 body 逐字相同。
`OK-ACCESS-TIMESTAMP` SHALL 為「本機時間 + OKX 校時偏移」格式化的 ISO 8601 UTC 毫秒字串（例如 `2020-12-08T09:08:57.715Z`）。OKX 從未校時成功時 SHALL 回報未連線（`ClockUnsynced`）且不送出請求。
OKX 回應 `50102`（時間戳過期）時，系統 SHALL 重新校時一次並只重送一次；再次被拒或校時失敗時 SHALL 原樣回報錯誤。

#### Scenario: 簽名與文件公式一致

- **WHEN** 以固定的 secret、時間戳 `2020-12-08T09:08:57.715Z` 與 `GET /api/v5/account/balance?ccy=BTC` 簽名
- **THEN** 結果等於以 Python `base64.b64encode(hmac.new(secret, prehash, sha256).digest())` 獨立算出的值

#### Scenario: 時間戳格式

- **WHEN** 本機時間為 1607418537000 毫秒、OKX 偏移為 +715 毫秒
- **THEN** `OK-ACCESS-TIMESTAMP` 為 `2020-12-08T09:08:57.715Z`

#### Scenario: 時間戳被拒後只重試一次

- **WHEN** 第一次請求回應 `50102`，重新校時成功，第二次仍回應 `50102`
- **THEN** 總共只送出兩次請求，並回報 `50102` 錯誤

### Requirement: 只接受合約模式或跨幣種保證金、且為單向持倉的 OKX 帳戶

系統 SHALL 以 `GET /api/v5/account/config` 讀取 `acctLv` 與 `posMode`。只有 `acctLv` 為 `2`（合約模式）或 `3`（跨幣種保證金）且 `posMode` 為 `net_mode` 時，OKX 帳戶資料 SHALL 視為可用。
其他組合 SHALL 使 OKX 的持倉、委託與可用保證金查詢回報「帳戶模式不支援」並指出實際的 `acctLv` / `posMode`，SHALL NOT 回傳空資料或 0。讀取失敗 SHALL 視為模式不明，處理方式相同。
符合的讀數 SHALL 最多沿用 60 秒；不符或讀取失敗的結果 SHALL NOT 被快取。系統 SHALL NOT 嘗試變更帳戶的模式設定。

#### Scenario: long/short 持倉模式

- **WHEN** `posMode` 為 `long_short_mode`
- **THEN** OKX 持倉查詢回報「帳戶模式不支援（long_short_mode）」，engine 的保證金與持倉檢查不通過

#### Scenario: 組合保證金

- **WHEN** `acctLv` 為 `4`
- **THEN** OKX 帳戶資料回報「帳戶模式不支援（acctLv 4）」

#### Scenario: 合約模式且單向

- **WHEN** `acctLv` 為 `2`、`posMode` 為 `net_mode`
- **THEN** 後續 60 秒內的查詢不再重讀帳戶設定

### Requirement: OKX 可用保證金取交易所自己計算的可用權益

`acctLv` 為 `2` 時，OKX 可用保證金 SHALL 取 `GET /api/v5/account/balance` 中 `details` 內 `ccy = USDT` 的 `availEq`；`acctLv` 為 `3` 時 SHALL 取帳戶層級的 `availEq`（USD 計價，視同 USDT）。
該欄位缺少、為空字串、或找不到 USDT 明細時 SHALL 回傳錯誤並指出欄位，SHALL NOT 以 0、`availBal`、`cashBal` 或任何自行計算的值代替；`"0"` 與負數 SHALL 原樣採用。

#### Scenario: 合約模式取 USDT 可用權益

- **WHEN** `acctLv` 為 `2`，USDT 明細 `availEq` 為 `"4834.31"`、`availBal` 為 `"5000"`
- **THEN** 可用保證金為 4834.31

#### Scenario: 跨幣種保證金取帳戶層級

- **WHEN** `acctLv` 為 `3`，帳戶層級 `availEq` 為 `"55415.62"`
- **THEN** 可用保證金為 55415.62

#### Scenario: 欄位為空

- **WHEN** 應採用的 `availEq` 為 `""`
- **THEN** 回傳指出 `availEq` 的錯誤，送單前保證金檢查不通過

### Requirement: OKX 持倉以張數交給 engine，換算幣量只在頁面層

系統 SHALL 以 `GET /api/v5/account/positions?instType=SWAP` 取得持倉，只保留 `-USDT-SWAP` 標的並轉為系統的 `BASEUSDT` 符號；`pos` 為 0 的列 SHALL 略過。
交給 engine 的 `AccountPosition.quantity` SHALL 為帶號的張數（多為正、空為負），與 `OrderRequest.quantity` 同單位。
提供給頁面的持倉數量 SHALL 為張數 × 該標的 `ctVal`（取自公開 instruments）；`ctVal` 未知時該列 SHALL 標示「無法換算」，SHALL NOT 以 1 或其他值代替。
任一列 `posSide` 為 `long` 或 `short`、或 `mgnMode` 為 `isolated` 時，整份持倉列表 SHALL 回報錯誤（系統假設單向、全倉），SHALL NOT 只略過該列。

#### Scenario: 空單以負張數交給 engine

- **WHEN** OKX 回報 `BTC-USDT-SWAP` 的 `pos` 為 `"-3"`、`posSide` 為 `net`、`mgnMode` 為 `cross`
- **THEN** engine 看到 `BTCUSDT` 持倉 −3（張），頁面在 `ctVal` 0.01 時顯示 −0.03 BTC

#### Scenario: 逐倉持倉使列表不可用

- **WHEN** 任一持倉列的 `mgnMode` 為 `isolated`
- **THEN** 持倉查詢回報錯誤並指出該標的，engine 不把它當成「沒有持倉」

### Requirement: OKX 未成交委託要取完所有頁

系統 SHALL 以 `GET /api/v5/trade/orders-pending?instType=SWAP` 取得未成交委託，以上一頁最後一筆的 `ordId` 作為 `after` 繼續分頁，直到某頁筆數少於 `limit`。
第一頁失敗 SHALL 回報錯誤；之後任一頁失敗、游標重複或達到頁數上限 SHALL 回傳已取得的項目並標示「不完整」與原因，SHALL NOT 當成完整列表。
委託數量與已成交數量 SHALL 保留張數，engine 的剩餘數量為 `sz − accFillSz`（張）。

#### Scenario: 第二頁失敗

- **WHEN** 第一頁回傳 100 筆，第二頁逾時
- **THEN** 回傳 100 筆並標示不完整，engine 的「無未成交委託」檢查不通過

#### Scenario: 不足一頁即完整

- **WHEN** 第一頁回傳 7 筆（`limit` 為 100）
- **THEN** 列表為完整，且只送出一個請求

### Requirement: engine 的 OKX 帳戶讀取使用 OKX 簽名客戶端

`AccountView` 對 OKX 的持倉、未成交委託與可用保證金 SHALL 由 OKX 簽名客戶端提供，SHALL NOT 再回傳「不支援」。OKX 未連線、帳戶模式不支援或查詢失敗時 SHALL 回傳 `Err` 並附原因（engine 依既有規則 fail closed）。
帳戶輪詢與交易頁的 `LegAccount` 輪詢 SHALL 包含 OKX。

#### Scenario: 缺金鑰時 engine 看到原因而非不支援

- **WHEN** OKX 沒有 passphrase，engine 查詢 OKX 可用保證金
- **THEN** 得到 `Err`，原因含 `NoPassphrase`，不含 "unsupported"
