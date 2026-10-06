## ADDED Requirements

### Requirement: OKX 訂單只以模擬交易身分送往寫死的主機

OKX 的送單、查單、撤單請求 SHALL 只經由 `okx-signed-read` 定義的 OKX 簽名網址取得途徑建構，並 SHALL 帶 `x-simulated-trading: 1`；執行器 SHALL NOT 提供覆寫主機或省略該標頭的途徑。
`execution/` 的原始碼 SHALL NOT 含任何主機字面或 `x-simulated-trading` 字面。
本 requirement 取代 `signed-order-execution` 中「OKX SHALL NOT 有簽名端點，也 SHALL NOT 下單」一句與「OKX 沒有下單能力」情境。

#### Scenario: 完整流程的每個請求都帶模擬標頭

- **WHEN** 以錄製用的傳輸層執行 OKX 的送單、查單、撤單
- **THEN** 每一個被記錄的請求主機都是寫死的 OKX 簽名主機，且都帶 `x-simulated-trading: 1`

#### Scenario: 下單傳輸層拒絕缺標頭的 OKX 請求

- **WHEN** 主機為 OKX 但沒有模擬標頭的請求交給真實下單傳輸層
- **THEN** 回傳錯誤且沒有建立任何連線

### Requirement: OKX 市價單的參數

OKX 送單 SHALL 為 `POST /api/v5/trade/order`，本文含：`instId`（`BASEUSDT` 轉為 `BASE-USDT-SWAP`）、`tdMode = cross`、`side`（`buy` / `sell`）、`ordType = market`、`sz`（core `Quantity` 的張數字串，SHALL NOT 在執行器內另行格式化或換算）、`clOrdId`（engine 的 `client_order_id`，原樣）、`reduceOnly`（明確的 `true` / `false`）。本文 SHALL NOT 含 `posSide`。
`client_order_id` 不是 1 到 32 個英數字時，SHALL 不送出並回報已拒絕（`not_sent`）。
送單前 SHALL 確認 OKX 帳戶模式讀數（`acctLv` 2 或 3 且 `net_mode`）在 60 秒內有效；無效、不符或讀取失敗時 SHALL 不送出並回報原因。

#### Scenario: 張數原樣送出

- **WHEN** engine 以數量 3（張）、`client_order_id` 為 `demolo0001abcdefghijklm12345678` 要求 OKX 買入 `BTCUSDT`
- **THEN** 被記錄的請求本文含 `"instId":"BTC-USDT-SWAP"`、`"sz":"3"`、`"clOrdId":"demolo0001abcdefghijklm12345678"`、`"tdMode":"cross"`、`"ordType":"market"`，且沒有 `posSide`

#### Scenario: clOrdId 含底線

- **WHEN** `client_order_id` 為 `demo_ab12`
- **THEN** 不送出任何請求，結果為已拒絕（`not_sent`），原因指出 OKX `clOrdId` 規則

#### Scenario: 帳戶改為 long/short 模式

- **WHEN** 帳戶模式讀數過期，重讀得到 `long_short_mode`
- **THEN** 不送出訂單，回報持倉模式不符

### Requirement: OKX 送單結果分類

每次 OKX 送單 SHALL 依序判讀傳輸結果、回應的 `code` 與 `data[0].sCode`，產生下列之一：
- 已接受：`code = "0"` 且 `sCode = "0"`，附 `ordId`；
- 被限流：HTTP 429，或 `code` / `sCode` 為 `50011`、`50061`；
- 結果未知：逾時、連線中斷、HTTP 5xx、回應無法解析，或 `code` / `sCode` 為 `50001`、`50004`、`50013`、`50026`；
- 已拒絕：其他明確的非零 `sCode`（附代碼與 `sMsg`），或 `code` 非零且無 `data`（例如 `50102`、`50113`）。
結果未知與被限流 SHALL 觸發以 `clOrdId` 查單確認，確認前意圖 SHALL NOT 標為失敗；執行器 SHALL NOT 以新的或相同的 id 重送下單。

#### Scenario: 端點逾時代碼是結果未知

- **WHEN** OKX 回應 HTTP 200、`code` 為 `"50004"`
- **THEN** 結果為未知，engine 以同一 `clOrdId` 查單，沒有第二次送單

#### Scenario: 餘額不足是已拒絕

- **WHEN** OKX 回應 `code` 為 `"1"`、`data[0].sCode` 為 `"51131"`
- **THEN** 結果為已拒絕，代碼 `51131`，訊息為 `sMsg`

#### Scenario: HTTP 200 但 sCode 非零

- **WHEN** `code` 為 `"0"` 但 `data[0].sCode` 為 `"51121"`
- **THEN** 結果為已拒絕，而非已接受

### Requirement: OKX 查單與撤單

查單 SHALL 為 `GET /api/v5/trade/order`，以 `instId` 加 `clOrdId`（或交易所 `ordId`）查詢；`51603` SHALL 視為查無。
訂單狀態對應：`live`、`partially_filled` → 未完成；`filled` → 已成交；`canceled`、`mmp_canceled` → 已撤銷。`filled_quantity` SHALL 為 `accFillSz`（張數），`avg_price` 為 `avgPx`（空字串為 `None`）。
手續費 SHALL 轉為 engine 的「正數 = 支付」：`fee = −(OKX fee)`，`fee_asset = feeCcy`（空字串為 `None`）。
撤單 SHALL 為 `POST /api/v5/trade/cancel-order`，只接受 `order_intents` 中存在的 `client_order_id`；撤單請求被接受、或回應 `51400` 時，SHALL 再查單一次並以查到的狀態回報。

#### Scenario: 已成交訂單的手續費正負號

- **WHEN** 查單回應 `state = filled`、`accFillSz = "3"`、`avgPx = "60010.5"`、`fee = "-0.9"`、`feeCcy = "USDT"`
- **THEN** 回報已成交 3（張）、均價 60010.5、手續費 0.9 USDT（支付）

#### Scenario: 查無訂單

- **WHEN** 查單回應 `code` 為 `"51603"`
- **THEN** 回報查無，而非查詢失敗

#### Scenario: 撤銷非本系統訂單

- **WHEN** 要求撤銷一個不在 `order_intents` 的 `client_order_id`
- **THEN** 不送出任何請求，回報撤單被拒

### Requirement: OKX 金鑰缺少不影響其他交易所的 demo 執行

切換到 EXCHANGE_DEMO 時，執行器工廠 SHALL 仍要求 Binance 與 Bybit 金鑰；OKX 金鑰（key、secret、passphrase）齊全時 SHALL 建立 OKX 客戶端，缺少時 SHALL 仍建立執行器，且其 OKX 訂單 SHALL 不送出並回報已拒絕（`not_sent`），原因 SHALL 指出缺少的項目（不含任何金鑰值）。

#### Scenario: 沒有 OKX 金鑰仍可在兩所 demo 交易

- **WHEN** Binance 與 Bybit 金鑰齊全，OKX 沒有 passphrase，切換到 EXCHANGE_DEMO
- **THEN** 切換成功；送往 OKX 的訂單為已拒絕（`not_sent`，原因 `NoPassphrase`），Binance 與 Bybit 的訂單照常送出

### Requirement: OKX 與 SimulatedExecutor 遵守同一個介面契約

OKX 執行路徑 SHALL 通過 `Executor` 介面契約測試的同一組情境（送單分類、以 id 查單、撤單只限自己的訂單、未知不重送），數量與成交量 SHALL 一律為張數。

#### Scenario: 契約測試涵蓋 OKX

- **WHEN** 執行 `executor_tests` 的介面契約測試
- **THEN** OKX 以錄製回應通過與 Binance、Bybit 相同的情境
