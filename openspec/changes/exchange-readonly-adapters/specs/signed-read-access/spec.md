## ADDED Requirements

### Requirement: 簽名請求只能抵達 demo/testnet 主機

所有簽名請求 SHALL 只送往編譯期常數所指定的 demo/testnet 主機（Binance USDS-M Futures demo/testnet、Bybit v5 Demo Trading）。
系統 SHALL NOT 提供任何方式（環境變數、設定檔、UI、命令列參數）把簽名請求導向正式環境主機。
因此即使使用者誤填真實帳戶的 API key，簽名請求也不會抵達真實資金的環境。

#### Scenario: 主機無法被外部改寫

- **WHEN** 在環境變數與設定檔中放入 `BINANCE_BASE_URL=https://fapi.binance.com`
- **THEN** 簽名請求仍只送往常數指定的 demo/testnet 主機，該環境變數不被讀取

#### Scenario: 傳輸層測試驗證目的地

- **WHEN** 以記錄請求目的地的假傳輸層執行 Binance 與 Bybit 的每一個簽名方法
- **THEN** 記錄到的所有目的主機都屬於 demo/testnet 常數集合

### Requirement: 簽名 GET 的範圍限於餘額、持倉與未成交委託

系統 SHALL 為 Binance 與 Bybit 各提供三個簽名 GET：帳戶餘額、持倉、未成交委託。
Binance 使用 `/fapi/v2/balance`、`/fapi/v2/positionRisk`、`/fapi/v1/openOrders`；Bybit 使用 `/v5/account/wallet-balance`、`/v5/position/list`、`/v5/order/realtime`。
本能力 SHALL NOT 包含任何 POST、DELETE 或其他會改變交易所狀態的簽名請求。
OKX SHALL NOT 有任何簽名請求的實作；對 OKX 查詢帳戶資料 SHALL 回傳「不支援」，而不是空資料。

#### Scenario: OKX 查詢帳戶

- **WHEN** 對 OKX 查詢餘額或持倉
- **THEN** 回傳「不支援（僅公開行情）」，呼叫端可據此顯示「僅比價」，且不發出任何請求

#### Scenario: 簽名模組沒有寫入型請求

- **WHEN** 掃描簽名客戶端模組中所有發出的 HTTP 方法
- **THEN** 只出現 GET

### Requirement: 金鑰取得失敗時視為未連線，且不發出請求

簽名客戶端 SHALL 只能經由 `secret-storage` 的單一存取介面取得金鑰。
取得失敗（項目不存在或讀取錯誤）時，該交易所 SHALL 為「未連線」狀態，簽名方法 SHALL 回傳 `NotConnected`，且 SHALL NOT 以空字串或預設值簽名、也 SHALL NOT 對外發出任何請求。
OKX 無簽名需求，其公開行情 SHALL NOT 受此規則影響。

#### Scenario: 金鑰不存在

- **WHEN** 以記憶體版金鑰存取介面（回報找不到 Bybit 金鑰）呼叫 Bybit 的持倉查詢
- **THEN** 回傳 `NotConnected`，傳輸層記錄到的請求數為 0

#### Scenario: Binance 金鑰缺失不影響其他所

- **WHEN** Binance 金鑰缺失、Bybit 金鑰存在
- **THEN** Bybit 的簽名查詢仍可執行，Binance 為「未連線」

### Requirement: 簽名時間戳使用校時後的時間且遵守 recvWindow

簽名請求的 `timestamp` SHALL 為「注入時鐘的目前時間 + 該所的 serverTime 偏移量」（見 `feed-health`），SHALL NOT 直接使用未校正的本機時間。
`recvWindow` SHALL 為 5000 毫秒（沿用 Python 版）。
該所尚未成功校時時，簽名請求 SHALL NOT 發出，並回傳 `NotConnected`（原因為時鐘未校時）。

#### Scenario: 使用偏移量

- **WHEN** 本機時間為 T、偏移量為 +1200 毫秒
- **THEN** 簽名的 `timestamp` 為 T + 1200

#### Scenario: 尚未校時不送簽名請求

- **WHEN** 某所尚無任何成功的校時紀錄
- **THEN** 簽名查詢回傳 `NotConnected`，傳輸層記錄到的簽名請求數為 0

### Requirement: 簽名查詢的結果標準化並帶有取得時間

餘額、持倉、未成交委託 SHALL 轉為標準化型別，每個結果 SHALL 帶有 `fetched_at`（收到回應的時間）與 `exchange`。
持倉 SHALL 只回傳數量不為零的列，並保留：標的、方向、數量（標的幣單位）、進場均價、標記價、槓桿、未實現損益、保證金（交易所有提供時）與持倉模式（one-way 或 hedge，取得不到時為未知）。
金額與數量 SHALL 以 Decimal 解析，SHALL NOT 經過浮點數。
餘額 SHALL 保留各幣種的數量與（交易所有提供時）USDT 估值，不得把非 USDT 幣種的數量直接當作 USDT。

#### Scenario: 持倉過濾零數量

- **WHEN** Binance `positionRisk` 回傳 300 列，其中 2 列 `positionAmt` 不為 0
- **THEN** 標準化結果只有 2 列

#### Scenario: 非 USDT 幣種不當作 USDT

- **WHEN** 餘額中有 `BTC` 0.05，且回應沒有提供 USDT 估值
- **THEN** 標準化結果保留 `BTC` 0.05 與「無估值」，而不是 50000 或 0.05 USDT

#### Scenario: 數量保留完整精度

- **WHEN** 回應的 `positionAmt` 為 `"0.019934"`
- **THEN** 標準化的數量為 Decimal 0.019934，無浮點誤差

### Requirement: Bybit 的持倉與委託列表要取完所有頁

Bybit 的持倉與未成交委託列表端點為分頁端點，系統 SHALL 依 `exchange-adapter` 的分頁規則取完所有頁，並明確指定最大 `limit`。
取不完整時，結果 SHALL 為 `Incomplete`，UI 與引擎 SHALL 能據此知道持倉列表可能缺列，不得當作完整持倉使用。

#### Scenario: 持倉超過單頁

- **WHEN** Bybit 持倉第一頁帶有 `nextPageCursor`，第二頁沒有
- **THEN** 結果包含兩頁的全部持倉

#### Scenario: 第二頁失敗

- **WHEN** 第二頁請求失敗
- **THEN** 結果為 `Incomplete`，並附上已取得的列，且呼叫端可辨識其不完整

### Requirement: 錯誤與日誌中不得出現金鑰或簽名

簽名請求產生的任何錯誤文字、日誌與事件 payload SHALL 先經 `secret-storage` 的遮蔽函式處理。
遮蔽 SHALL 涵蓋 URL 查詢參數 `signature` 與標頭 `X-MBX-APIKEY`、`X-BAPI-API-KEY`、`X-BAPI-SIGN`。

#### Scenario: 連線逾時的錯誤字串含簽名

- **WHEN** Binance 簽名請求逾時，底層錯誤字串含 `...&signature=abcdef`
- **THEN** 回傳的 `AdapterError` 文字與寫入事件的 payload 中，`signature` 的值為占位文字
