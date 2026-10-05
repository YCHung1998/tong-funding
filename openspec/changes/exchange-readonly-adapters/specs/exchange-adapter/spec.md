## ADDED Requirements

### Requirement: ExchangeAdapter 是唯讀介面，統一輸出標準化型別

系統 SHALL 以單一 `ExchangeAdapter` trait 表示一個交易所的唯讀存取，三所（Binance、Bybit、OKX）各有一個實作。
trait 的方法 SHALL 只包含讀取：批次行情快照、單一標的重新抓取、標的規則（lot size 等）、上市狀態。
trait 與其實作 SHALL NOT 含有下單、撤單、改單、改槓桿等任何會改變交易所狀態的方法。
各所輸出 SHALL 統一為標準化型別：`FundingObservation`（沿用 `funding-observation` 的欄位，含 `funding_interval_secs`、雙時間戳與 `data_status`）與 `InstrumentRules`；各所特有的欄位名稱與單位 SHALL 在 adapter 內部消化，不得外洩到 adapter 以外。

#### Scenario: 只需實作唯讀方法即可滿足 trait

- **WHEN** 開發者寫一個測試替身，只實作 trait 的全部方法
- **THEN** 該替身可通過編譯並用於測試，且 trait 的方法清單中找不到任何下單或撤單方法

#### Scenario: 三所輸出同一種型別

- **WHEN** 分別對三所的錄製回應執行解析
- **THEN** 三者都產出同型別的 `FundingObservation`，欄位的單位一致（rate 為小數、價格與成交量為 Decimal）

### Requirement: 所有失敗以型別化的 AdapterError 回傳

adapter 的每個方法 SHALL 回傳 `Result`，錯誤 SHALL 為封閉的 `AdapterError` 列舉，至少區分：`Timeout`、`Network`（連線、DNS、TLS）、`Http { status }`、`RateLimited { retry_after }`、`Exchange { code, message }`（HTTP 200 但回應本文標示失敗，例如 Bybit `retCode ≠ 0`、OKX `code ≠ "0"`）、`Parse`、`Incomplete`、`NotConnected`。
任何網路層或解析層的例外 SHALL 被轉換為上述其中一種，SHALL NOT 以 panic 結束，也 SHALL NOT 被吞掉而回傳空結果。
錯誤的文字內容 SHALL 在離開 adapter 前通過 `secret-storage` 規定的遮蔽函式。

#### Scenario: 逾時被轉為 Timeout

- **WHEN** 對某所的請求超過該請求的逾時上限仍無回應
- **THEN** 方法回傳 `Timeout`，而不是 panic、也不是空的成功結果

#### Scenario: HTTP 200 但交易所回報失敗

- **WHEN** Bybit 回傳 HTTP 200 且本文 `retCode` 為非零值
- **THEN** 方法回傳 `Exchange { code, message }`，不得當作成功

#### Scenario: 回應不是合法 JSON

- **WHEN** 某所回傳無法解析的本文
- **THEN** 方法回傳 `Parse`，且錯誤文字不含任何 API key 或簽名

### Requirement: 公開行情與簽名請求使用結構上分離的 HTTP 客戶端

公開（不需簽名）行情 SHALL 只經由「公開客戶端」型別發出，該型別 SHALL NOT 能讀取金鑰、也 SHALL NOT 能產生簽名。
簽名請求 SHALL 只經由「簽名客戶端」型別發出，其可連線的主機 SHALL 為編譯期常數，且 SHALL 僅為 Binance Futures demo/testnet 主機與 Bybit demo 主機。
簽名客戶端 SHALL NOT 提供接受任意 base URL 的建構方式，也 SHALL NOT 從環境變數或設定檔讀取主機。
正式環境主機名稱（`fapi.binance.com`、`api.bybit.com`、`www.okx.com`）SHALL 只出現在公開客戶端模組。

#### Scenario: 原始碼靜態掃描

- **WHEN** 掃描 `app` 內簽名客戶端模組的原始碼
- **THEN** 找不到正式環境主機名稱，也找不到接受外部 base URL 的建構函式

#### Scenario: 公開客戶端無法簽名

- **WHEN** 檢查公開客戶端型別的欄位與方法
- **THEN** 其中沒有金鑰、secret、簽名函式或 `Authorization` 類標頭的處理

### Requirement: Funding 週期由 API 推導，查不到標為 DATA_ERROR

`funding_interval_secs` SHALL 由各所 API 推導：Binance 取 `GET /fapi/v1/fundingInfo` 的 `fundingIntervalHours` 乘以 3600；
Bybit 取 `GET /v5/market/instruments-info` 的 `fundingInterval`（單位：分鐘）乘以 60；
OKX 取 `GET /api/v5/public/funding-rate` 的 `nextFundingTime − fundingTime`（毫秒）換算為秒。
查不到、為零或為負時，該標的的 `data_status` SHALL 為 `DATA_ERROR`，且 SHALL NOT 以 28800 秒或任何預設值代替。
週期查詢結果 SHALL 以標的為鍵快取，快取 SHALL 記錄取得時間；週期資料整批取得失敗時，所有受影響標的 SHALL 為 `DATA_ERROR`，不得沿用逾期的舊值而不標示。

#### Scenario: Binance 的週期

- **WHEN** `fundingInfo` 對 `GTCUSDT` 回報 `fundingIntervalHours = 8`，對另一標的回報 `4`
- **THEN** 前者為 28800 秒、後者為 14400 秒

#### Scenario: Bybit 的週期以分鐘換算

- **WHEN** `instruments-info` 對某永續合約回報 `fundingInterval = 240`
- **THEN** `funding_interval_secs` 為 14400

#### Scenario: Bybit 到期型合約的週期為 0

- **WHEN** `instruments-info` 對 `contractType = LinearFutures` 的合約回報 `fundingInterval = 0`
- **THEN** 該合約不被當成永續合約處理；若它被要求產生觀測，`data_status` 為 `DATA_ERROR`

#### Scenario: OKX 的週期由時間差推導

- **WHEN** `fundingTime = 1791216000000`、`nextFundingTime = 1791244800000`
- **THEN** `funding_interval_secs` 為 28800

#### Scenario: Binance 查不到該標的的週期

- **WHEN** `fundingInfo` 的回應中沒有某個交易中標的
- **THEN** 該標的 `data_status` 為 `DATA_ERROR`，`funding_interval_secs` 不被填入 28800

### Requirement: 週期與下次結算時間不一致時標為 DATA_ERROR

快取的週期可能比行情資料舊（交易所調整某標的的結算週期之後）。系統 SHALL 在每筆觀測檢查：
`next_funding_time − 參考時間` 不得大於 `funding_interval_secs` 加上容差，其中參考時間取 `exchange_timestamp`，缺少時取 `observed_at`。
違反時該觀測的 `data_status` SHALL 為 `DATA_ERROR`，並 SHALL 觸發該所週期資料的重新取得。
Bybit 的 `tickers` 若同時回報 `fundingIntervalHour`，其值（小時）與 `instruments-info` 推導的週期不一致時，該標的 SHALL 為 `DATA_ERROR`；`tickers` 沒有此欄位時略過此檢查。

#### Scenario: 週期快取過舊

- **WHEN** 快取中某標的週期為 4 小時，但最新行情的下次結算時間距參考時間為 7 小時
- **THEN** 該觀測為 `DATA_ERROR`，並觸發週期資料重新取得

#### Scenario: 一致的資料

- **WHEN** 週期為 8 小時，下次結算時間距參考時間為 7.05 小時
- **THEN** 檢查通過，`data_status` 不因此變動

#### Scenario: Bybit 兩個來源的週期不一致

- **WHEN** `instruments-info` 推導為 480 分鐘，同一標的的 `tickers.fundingIntervalHour` 為 `"4"`
- **THEN** 該標的為 `DATA_ERROR`

### Requirement: 下次結算時間的取值與 OKX 的欄位語意

`next_funding_time` SHALL 為「下一次即將結算」的時間：Binance 取 `premiumIndex.nextFundingTime`、Bybit 取 `tickers.nextFundingTime`、OKX 取 `funding-rate.fundingTime`。
OKX 的 `nextFundingTime` 是「再下一次」的結算時間，SHALL NOT 當作下次結算時間使用，只用於推導週期。
OKX 的 `fundingRate` 為 `fundingTime` 那次結算適用的費率（回應 `method` 為 `current_period`）；若 `method` 不是 `current_period`，該標的 SHALL 標為 `DATA_ERROR`。

#### Scenario: OKX 取 fundingTime

- **WHEN** OKX 回報 `fundingTime = 1791216000000`、`nextFundingTime = 1791244800000`、`prevFundingTime = 1791187200000`
- **THEN** `next_funding_time` 為 1791216000000

#### Scenario: OKX 的費率計算方式不是 current_period

- **WHEN** OKX 對某標的回報 `method` 為 `current_period` 以外的值
- **THEN** 該標的 `data_status` 為 `DATA_ERROR`

### Requirement: 只納入實際可交易的 USDT 永續合約

批次行情端點會回傳不可交易的標的。系統 SHALL 以各所的合約目錄過濾，只有同時符合下列條件的標的才可為 `LISTED`：
Binance：`exchangeInfo` 的 `status = TRADING`、`contractType = PERPETUAL`、`quoteAsset = USDT`；
Bybit：`instruments-info` 的 `status = Trading`、`contractType = LinearPerpetual`、`quoteCoin = USDT`；
OKX：`instruments` 的 `state = live`、`ctType = linear`，且 `instId` 以 `-USDT-SWAP` 結尾。
行情端點有回傳、但不在上述集合內的標的，其 `data_status` SHALL NOT 為 `LISTED`（為 `NOT_LISTED`），且 SHALL NOT 參與任何比價或達標判定。
合約目錄無法取得時，所有標的 SHALL 為 `DATA_ERROR`，不得全部視為 `LISTED`。

#### Scenario: 結算中的標的被排除

- **WHEN** Binance `premiumIndex` 回傳某標的，但 `exchangeInfo` 中該標的 `status` 為 `SETTLING`
- **THEN** 該標的為 `NOT_LISTED`

#### Scenario: Bybit 到期型合約被排除

- **WHEN** `instruments-info` 的某合約 `contractType` 為 `LinearFutures`
- **THEN** 該合約不為 `LISTED`

#### Scenario: 合約目錄失敗不放行

- **WHEN** 取得 `exchangeInfo` 失敗，但 `premiumIndex` 成功
- **THEN** 該所所有標的的 `data_status` 為 `DATA_ERROR`，而不是 `LISTED`

### Requirement: 分頁端點必須取完或明確標示不完整

凡是回應帶有分頁 cursor 的批次端點（至少包含 Bybit `instruments-info` 與簽名的持倉與委託列表），系統 SHALL 持續以回應的 cursor 取下一頁，直到 cursor 為空。
請求 SHALL 明確指定該端點允許的最大 `limit`，不得依賴預設頁大小。
任何一頁失敗、cursor 重複出現（可能無限迴圈）、或超過頁數上限時，結果 SHALL 為 `Incomplete`；
`Incomplete` 的目錄 SHALL NOT 被用來判定某標的為 `NOT_LISTED`，缺少的標的 SHALL 為 `DATA_ERROR`。

#### Scenario: 預設頁大小會截斷時仍取完

- **WHEN** Bybit `instruments-info` 第一頁回傳 500 筆且 `nextPageCursor` 非空，第二頁回傳其餘筆且 cursor 為空
- **THEN** 結果包含兩頁合併後的全部筆數，狀態為完整

#### Scenario: 中途失敗標為不完整

- **WHEN** 第二頁請求失敗
- **THEN** 結果為 `Incomplete`，第一頁中沒有出現的標的為 `DATA_ERROR`，不被判為 `NOT_LISTED`

#### Scenario: cursor 重複時中止

- **WHEN** 連續兩頁回傳相同的 cursor
- **THEN** 迴圈中止並回傳 `Incomplete`，而不是無限請求

### Requirement: 24h 成交量統一為 USDT 計價

`volume_24h_quote` SHALL 為以 USDT 計價的 24 小時成交額：Binance 取 `ticker/24hr` 的 `quoteVolume`；Bybit 取 `tickers` 的 `turnover24h`；
OKX 的 `tickers` 沒有 USDT 成交額，其 `volCcy24h` 為以標的幣計的成交量，SHALL 乘以同一筆 ticker 的 `last` 換算為 USDT；`last` 缺失時 `volume_24h_quote` SHALL 為空（由 `net-edge` 的規則視為 0），SHALL NOT 直接把幣量當作 USDT。
換算所用公式與近似性 SHALL 在 `FundingObservation` 附近的文件註記。

#### Scenario: OKX 的幣量換算為 USDT

- **WHEN** OKX ticker 回報 `volCcy24h = 100`、`last = 50`
- **THEN** `volume_24h_quote` 為 5000

#### Scenario: OKX 缺少 last

- **WHEN** OKX ticker 的 `last` 為空
- **THEN** `volume_24h_quote` 為空，而不是 100

### Requirement: 單一標的的重新抓取不得使用任何快取

adapter SHALL 提供「單一標的重新抓取」方法，供送單前檢查與「立即刷新」使用。
該方法 SHALL 每次都發出新的 HTTP 請求（Binance `premiumIndex?symbol=`、Bybit `tickers?symbol=`、OKX `funding-rate?instId=` 與 `mark-price`），SHALL NOT 讀取輪詢快取或 WebSocket 快取。
回傳的 `observed_at` SHALL 為收到回應的時間（來自注入的時鐘）；同一次重新抓取需要多個請求時，`observed_at` SHALL 取其中最早的收到時間；`exchange_timestamp` SHALL 取自回應本文。
批次的「立即刷新」同樣 SHALL 繞過輪詢快取，對每一個啟用的來源重新發出請求。

#### Scenario: 快取存在時仍發出新請求

- **WHEN** 輪詢快取中有 8 秒前的某標的資料，呼叫單一標的重新抓取
- **THEN** 傳輸層記錄到一次新的請求，回傳的 `observed_at` 晚於快取中的 `observed_at`

#### Scenario: 多個請求取最早的收到時間

- **WHEN** 一次重新抓取的兩個請求分別在 T 與 T+300 毫秒收到回應
- **THEN** 回傳的 `observed_at` 為 T

### Requirement: 標的規則包含各所的數量約束

`InstrumentRules` SHALL 包含各所下單所需的數量約束，供 `quantity-precision` 使用：
Binance 的 `LOT_SIZE` 與 `MARKET_LOT_SIZE`（`stepSize`、`minQty`、`maxQty`）；Bybit 的 `lotSizeFilter`（`qtyStep`、`minOrderQty`、`maxMktOrderQty`、`minNotionalValue`）；OKX 的 `ctVal`、`ctMult`、`lotSz`、`minSz`（單位為合約張數）。
欄位缺失時，該標的的 `InstrumentRules` SHALL 為不可用，呼叫端 SHALL NOT 以預設步長代替。

#### Scenario: Binance 兩組數量約束都保留

- **WHEN** `exchangeInfo` 的某標的同時有 `LOT_SIZE`（`maxQty = 1000`）與 `MARKET_LOT_SIZE`（`maxQty = 120`）
- **THEN** 兩組數值都出現在 `InstrumentRules` 中，不互相覆蓋

#### Scenario: 欄位缺失不使用預設步長

- **WHEN** 某標的的 `lotSizeFilter` 缺少 `qtyStep`
- **THEN** 該標的的 `InstrumentRules` 為不可用
