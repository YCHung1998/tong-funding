## ADDED Requirements

### Requirement: 簽名請求只能送往寫死的 demo/testnet 端點

簽名下單、撤單、查單的請求 SHALL 只能送往程式中寫死的 demo/testnet 主機；執行器 SHALL NOT 提供任何由設定、環境變數或使用者輸入覆寫主機的途徑。
程式碼與設定中 SHALL NOT 存在任何真錢交易端點。OKX SHALL NOT 有簽名端點，也 SHALL NOT 下單。

#### Scenario: 所有請求的主機都在允許清單內

- **WHEN** 以錄製用的傳輸層執行完整的送單、查單、撤單流程
- **THEN** 每一個被記錄的請求主機都屬於寫死的 demo/testnet 允許清單

#### Scenario: 無法覆寫主機

- **WHEN** 嘗試以環境變數或設定值把基底網址改為其他主機
- **THEN** 執行器忽略該值，仍只使用寫死的主機，或建構失敗

#### Scenario: OKX 沒有下單能力

- **WHEN** 檢查執行器的公開介面與 OKX 模組
- **THEN** 找不到任何 OKX 簽名請求或下單函式

### Requirement: 每一單帶 client order id

每筆送出的訂單 SHALL 帶有由引擎提供的 `client_order_id`：Binance 以 `newClientOrderId` 傳送、Bybit 以 `orderLinkId` 傳送。
執行器 SHALL NOT 自行產生或更改該 id；沒有 `client_order_id` 的送單請求 SHALL 在型別層被拒絕（無法建構）。

#### Scenario: Binance 帶 newClientOrderId

- **WHEN** 引擎以 `client_order_id` 為 `demo_ab12_L_open_1` 要求 Binance 送單
- **THEN** 被記錄的請求參數含 `newClientOrderId=demo_ab12_L_open_1`

#### Scenario: Bybit 帶 orderLinkId

- **WHEN** 引擎以同一 id 要求 Bybit 送單
- **THEN** 被記錄的請求本文含 `orderLinkId` 且值相同

### Requirement: 數量一律來自 Quantity，並確認持倉模式

送單的數量字串 SHALL 只來自 core 的 `Quantity`（開倉為取整後的數量、平倉為由交易所持倉建構的數量），SHALL NOT 在執行器內另行格式化。
執行器 SHALL 在送出任何訂單前確認帳戶的持倉模式符合系統的假設（單向持倉）；模式不明或不符時 SHALL 拒絕送單並回報原因。

#### Scenario: 持倉模式不符

- **WHEN** 帳戶回報的持倉模式與系統假設不符
- **THEN** 不送出任何訂單，並回報持倉模式不符

#### Scenario: 持倉模式查詢失敗

- **WHEN** 查詢持倉模式時逾時
- **THEN** 不送出任何訂單，視為模式不明

### Requirement: 送單結果必須分類，未知結果不得被吞掉或當成失敗

每次送單 SHALL 產生下列結果之一：已接受（附交易所 order id）、已拒絕（交易所明確回覆拒單，附錯誤碼與訊息）、被限流、結果未知。
逾時、連線中斷、回應無法解析 SHALL 一律歸為「結果未知」，SHALL NOT 被歸為「已拒絕」，也 SHALL NOT 以通用錯誤字串帶過。
Bybit 回應 HTTP 200 但 `retCode` 非 0 時 SHALL 歸為「已拒絕」。
「結果未知」與「被限流」 SHALL 觸發以 `client_order_id` 查單確認，確認前意圖 SHALL NOT 被標為失敗。

#### Scenario: 逾時是結果未知

- **WHEN** 送單請求在逾時內沒有收到回應
- **THEN** 結果為「結果未知」，意圖維持已送出，並以 `client_order_id` 查單

#### Scenario: 連線中斷

- **WHEN** 送單請求因連線被重置而失敗
- **THEN** 結果為「結果未知」，而不是「已拒絕」

#### Scenario: Bybit HTTP 200 但 retCode 非 0

- **WHEN** Bybit 回應 HTTP 200 與非 0 的 `retCode`
- **THEN** 結果為「已拒絕」，附 `retCode` 與 `retMsg`

#### Scenario: 被限流

- **WHEN** 交易所回應 HTTP 429 並帶有 `Retry-After`
- **THEN** 結果為「被限流」，後續請求依 `Retry-After` 退避，且引擎以 `client_order_id` 查單確認該單確實未成立

### Requirement: 兩腿並行送出並記錄 request、ACK 與延遲

兩腿的送單 SHALL 並行發出：第二腿的請求 SHALL NOT 等待第一腿回應後才送出。
每一腿 SHALL 記錄 `request_sent_at`、`ack_at`（收到回應時）與 `latency_ms`，連同 `client_order_id`、交易所 order id（若有）與結果分類，寫成不可變事件。
時間 SHALL 取自注入的時鐘，事件 SHALL 經過機敏資訊遮蔽。

#### Scenario: 兩腿並行

- **WHEN** 兩個交易所的傳輸層各以 200 毫秒（假時鐘）回應
- **THEN** 兩個請求都在任一回應之前已送出，且整體耗時約 200 毫秒而非 400 毫秒

#### Scenario: 延遲事件

- **WHEN** 某腿請求於 t=0 送出、於 t=180 毫秒收到 ACK
- **THEN** 事件記錄 `latency_ms` 為 180，並含 `client_order_id`

#### Scenario: 事件不含機敏資訊

- **WHEN** 掃描產生的事件與日誌
- **THEN** 不含簽名、API key 與 secret 字串

### Requirement: 可依 client_order_id 或交易所 order id 查單，且撤單只限自己送出的訂單

執行器 SHALL 提供依 `client_order_id` 與依交易所 order id 查詢單一訂單狀態與累計成交量的能力，供成交確認與重啟對帳使用。
撤單 SHALL 只接受 `order_intents` 中存在的 `client_order_id`；交易所上不屬於本系統的委託 SHALL NOT 被撤銷。

#### Scenario: 撤銷不屬於系統的委託

- **WHEN** 對一個不在 `order_intents` 的 order id 要求撤單
- **THEN** 請求被拒絕，沒有任何撤單請求送出

#### Scenario: 以 client id 查單

- **WHEN** 重啟對帳以某 `client_order_id` 查單，交易所回報該單已完整成交
- **THEN** 回傳狀態與累計成交量，供對帳使用

### Requirement: 與 SimulatedExecutor 遵守同一個介面契約

真實執行器 SHALL 與 `SimulatedExecutor` 實作同一個 `Executor` 介面，並 SHALL 通過同一組契約測試（結果分類、`client_order_id` 唯一性、查單與撤單語意）。
真實執行器 SHALL 只由「切換至 `EXCHANGE_DEMO`」的工廠建立，且金鑰讀取失敗時 SHALL 建構失敗。

#### Scenario: 同一組契約測試

- **WHEN** 對兩種實作各跑一次契約測試套件
- **THEN** 兩者皆通過

#### Scenario: 金鑰不可用

- **WHEN** 工廠建立真實執行器時讀不到金鑰
- **THEN** 建構失敗並回報未連線，引擎維持 `SIMULATION`
