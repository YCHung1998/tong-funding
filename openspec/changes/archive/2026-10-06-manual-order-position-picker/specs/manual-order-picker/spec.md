## ADDED Requirements

### Requirement: 手動下單頁列出目前持倉並可帶入平倉單

手動下單頁 SHALL 列出目前 `execution_mode` 對應帳戶（`SIMULATION` 為模擬帳戶、`EXCHANGE_DEMO` 為 demo 帳戶）在 Binance 與 Bybit 上的所有非零持倉，每列顯示交易所、Symbol、方向（多 / 空）與數量。
每列 SHALL 提供「帶入平倉」操作；點選後表單 SHALL 被填入：該交易所、該 Symbol、與持倉相反的方向（多 → SELL、空 → BUY）、數量 = 持倉數量絕對值、`reduce_only` = 勾選。
帶入 SHALL NOT 直接送單；送出仍 SHALL 經過既有的取整與確認流程。

#### Scenario: 帶入空單平倉

- **WHEN** 模擬帳戶在 Bybit 有 `ETHUSDT` 數量 −0.5 的持倉，使用者點該列「帶入平倉」
- **THEN** 表單變為 Bybit、`ETHUSDT`、BUY、0.5、reduce_only 勾選，且尚未送出任何命令

#### Scenario: 依模式取帳戶

- **WHEN** `execution_mode` 為 `EXCHANGE_DEMO`，模擬帳戶與 demo 帳戶持倉不同
- **THEN** 清單只顯示 demo 帳戶的持倉

#### Scenario: 沒有持倉

- **WHEN** 對應帳戶沒有任何非零持倉
- **THEN** 清單顯示「目前沒有持倉」

### Requirement: 持倉清單如實反映資料狀態

某交易所的持倉讀取失敗時，清單 SHALL 顯示該交易所「持倉讀取失敗」與錯誤訊息；回報為不完整（`complete = false`）時 SHALL 標示「清單可能不完整」。
系統 SHALL NOT 以先前的成功結果替代失敗的讀取。

#### Scenario: 讀取失敗

- **WHEN** Binance 的持倉讀取回傳錯誤
- **THEN** 清單出現「Binance 持倉讀取失敗」，且不顯示任何 Binance 持倉列

### Requirement: 撤單可從目前掛單帶入

撤單區 SHALL 列出對應帳戶在 Binance 與 Bybit 上的掛單（Symbol、方向、數量、Order ID）；點選某列 SHALL 填入撤單表單的交易所、Symbol 與 Order ID。
沒有 `client_order_id` 的掛單 SHALL 顯示但不可點選，並說明「無 Order ID，無法由此撤單」。

#### Scenario: 帶入撤單

- **WHEN** 使用者點選 Binance `BTCUSDT`、Order ID `tf-123` 的掛單
- **THEN** 撤單表單變為 Binance、`BTCUSDT`、`tf-123`，尚未送出
