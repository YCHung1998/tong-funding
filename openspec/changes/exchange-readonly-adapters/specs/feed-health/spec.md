## ADDED Requirements

### Requirement: 與各交易所 serverTime 校時並保存偏移量

系統 SHALL 對每個交易所以其公開時間端點校時：Binance `GET /fapi/v1/time`、Bybit `GET /v5/market/time`、OKX `GET /api/v5/public/time`。
偏移量 SHALL 以「交易所時間 − 請求送出與收到回應兩個本機時間的中點」計算，並與該次往返時間（RTT）一併保存；偏移量計算 SHALL 為接受注入時間的純函式。
系統 SHALL 在啟動時校時，並定期重新校時；校時失敗時 SHALL 沿用上一次成功的偏移量並標示其年齡，從未成功校時的交易所 SHALL 標為「時鐘未校時」。
收到交易所因時間戳被拒絕的回應（例如 Binance 的 recvWindow 錯誤）時，SHALL 立即重新校時並僅重試該 GET 一次。
偏移量的絕對值大於 `recvWindow` 的一半時，SHALL 產生警示（供 `alert-banner` 使用）。

#### Scenario: 偏移量的計算

- **WHEN** 請求在本機時間 1000 送出、在 1200 收到回應，交易所時間為 1700
- **THEN** 偏移量為 600（1700 − (1000 + 1200) ÷ 2），RTT 為 200

#### Scenario: 校時失敗沿用舊偏移量

- **WHEN** 先前成功校時（偏移量 +300），本次校時請求逾時
- **THEN** 偏移量仍為 +300，並附上其距今的年齡，狀態不是「未校時」

#### Scenario: 從未校時

- **WHEN** 啟動後第一次校時失敗
- **THEN** 該交易所為「時鐘未校時」，簽名請求不得發出

#### Scenario: 時間戳被拒絕後重新校時一次

- **WHEN** 簽名 GET 因時間戳超出 recvWindow 被拒絕
- **THEN** 系統重新校時並重送該 GET 一次；若再次被拒絕則回傳錯誤，不再重試

### Requirement: 限流時遵守 Retry-After 並以退避重試

遇到限流回應（HTTP 429、Binance 的 418、Bybit 與 OKX 於文件所載的限流代碼）時，adapter SHALL 回傳 `RateLimited { retry_after }`。
有 `Retry-After` 標頭時 SHALL 以其值為最短等待時間；沒有時 SHALL 使用指數退避，且有上限。
退避期間，針對同一所的同一類請求 SHALL NOT 發出，直到等待時間結束；退避狀態 SHALL 能被查詢並顯示。
連續失敗後的第一次成功 SHALL 重設退避。
Binance 回應的已用權重標頭（`x-mbx-used-weight-1m`）SHALL 被讀取；已用權重達到 `exchangeInfo.rateLimits` 所列 `REQUEST_WEIGHT` 上限的 80% 時，系統 SHALL 暫緩非必要的批次輪詢，必要請求（送單前重新抓取）不受此暫緩影響。
上限值 SHALL 取自 `exchangeInfo.rateLimits`，不得寫死。

#### Scenario: 429 帶 Retry-After

- **WHEN** 某所回 429 且 `Retry-After: 5`
- **THEN** 回傳 `RateLimited { retry_after = 5s }`，並且在 5 秒內不對該所同類請求發出新請求

#### Scenario: 429 沒有 Retry-After

- **WHEN** 連續三次 429 且都沒有 `Retry-After`
- **THEN** 三次的等待時間呈指數成長且不超過上限

#### Scenario: 成功後重設退避

- **WHEN** 退避期結束後的請求成功
- **THEN** 退避狀態歸零，下一次失敗從最小等待時間重新開始

#### Scenario: 權重接近上限時暫緩批次輪詢

- **WHEN** `rateLimits` 的 `REQUEST_WEIGHT` 上限為 2400，已用權重回報為 1950
- **THEN** 非必要的批次輪詢被暫緩，而送單前的單一標的重新抓取仍可發出

### Requirement: Binance 全市場 mark price WebSocket 的接收與解析

系統 SHALL 訂閱 Binance 全市場 mark price 串流的 1 秒版本（`!markPrice@arr@1s`），以取得各標的的 funding rate、下次結算時間、mark price 與事件時間。
解析 SHALL 取 `s`（標的）、`r`（funding rate）、`T`（下次結算時間）、`p`（mark price）、`E`（事件時間，作為 `exchange_timestamp`）；缺少必要欄位的項目 SHALL 被略過，不得中斷整則訊息的處理。
單則訊息可能只含部分標的，系統 SHALL 只更新該訊息出現的標的，SHALL NOT 清除其他標的。
每個標的的 `observed_at` SHALL 為收到含該標的之訊息的時間。

#### Scenario: 解析一則訊息

- **WHEN** 收到含 `{"s":"BTCUSDT","p":"86186.20","r":"0.00005047","T":1791216000000,"E":1791190833000}` 的訊息
- **THEN** BTCUSDT 的 rate 為 0.00005047、下次結算為 1791216000000、mark price 為 86186.20、`exchange_timestamp` 為 1791190833000

#### Scenario: 部分更新不清除其他標的

- **WHEN** 先收到含 745 個標的的訊息，再收到只含 213 個標的的訊息
- **THEN** 前一則中不在後一則的標的仍保留各自的資料與各自的 `observed_at`

#### Scenario: 缺欄位的項目被略過

- **WHEN** 訊息中某項目缺少 `r`
- **THEN** 該項目被略過，同一則訊息中其他項目仍被處理

### Requirement: 資料來源的連線狀態與新鮮度判定

系統 SHALL 為每個資料來源（Binance WebSocket、各所的 REST 輪詢、各所的簽名帳戶輪詢）維護健康狀態，狀態 SHALL 至少包含：已連線或已斷線、最後一次成功更新的時間、連續失敗次數、預期的更新週期。
來源 SHALL 在下列情況被判定為「過期」：從未成功更新、已斷線、或「目前時間 − 最後一次成功更新時間」大於 `max(stale_data_threshold_ms, 3 × 預期更新週期)`。
WebSocket 的 close 或 error 事件 SHALL 立即使該來源為已斷線，SHALL NOT 等到計時器逾時才改變狀態。
連線仍在但超過上述門檻沒有收到任何訊息時，SHALL 視為已斷線並重新連線。
重新連線 SHALL 以指數退避（起點 1 秒、上限 30 秒，沿用 Python 版）進行。
未知的健康狀態 SHALL 被視為異常，而不是正常。

#### Scenario: 斷線事件立即生效

- **WHEN** WebSocket 回報 close 事件，距離上一則訊息只有 200 毫秒
- **THEN** 來源狀態立即為已斷線與過期，不等待任何計時器

#### Scenario: 連線存在但沒有訊息

- **WHEN** 預期更新週期為 1000 毫秒，`stale_data_threshold_ms` 為 1000，已超過 3000 毫秒沒有收到訊息
- **THEN** 來源被判定為過期並觸發重新連線

#### Scenario: REST 輪詢未超過門檻

- **WHEN** Bybit 輪詢週期為 10 秒（門檻為 max(1 秒, 30 秒) = 30 秒），最後一次成功更新在 25 秒前
- **THEN** 來源不被判定為過期

#### Scenario: REST 輪詢超過門檻

- **WHEN** 同上條件，最後一次成功更新在 31 秒前
- **THEN** 來源被判定為過期

#### Scenario: 從未成功

- **WHEN** 啟動後某來源一次都沒有成功更新
- **THEN** 該來源為過期，而不是正常

### Requirement: 快取讀取介面不得只回傳價格

任何讀取快取價格的介面 SHALL 同時回傳該筆資料的 `observed_at`、`exchange_timestamp` 與所屬來源當下的健康狀態；SHALL NOT 存在只回傳價格數值的讀取方法。
來源為已斷線或過期時，依其快取所得的資料 SHALL NOT 被標為新鮮，也 SHALL NOT 被送單前檢查採用；送單前檢查 SHALL 只使用 `exchange-adapter` 的單一標的重新抓取結果。

#### Scenario: WebSocket 斷線後讀快取

- **WHEN** WebSocket 已斷線 5 秒，呼叫端讀取某標的的快取價格
- **THEN** 回傳值包含來源狀態為已斷線與 5 秒前的 `observed_at`，呼叫端無法在不處理狀態的情況下取得單獨的價格

#### Scenario: 送單前檢查不採用快取

- **WHEN** 送單前檢查需要某標的的價格，而快取中有資料
- **THEN** 檢查使用的是單一標的重新抓取的結果，而不是快取

### Requirement: 健康狀態轉換才寫入事件，不逐次記錄

資料來源的健康狀態發生轉換（正常 → 失敗、失敗 → 恢復、進入或離開限流、時鐘未校時）時，系統 SHALL 寫入事件到 `events`；
同一種持續中的失敗在每次輪詢重複發生時，SHALL NOT 重複寫入事件。
進入失敗 SHALL 寫入 `FETCH_ERROR`（含來源、錯誤類型與遮蔽後的訊息），恢復 SHALL 寫入另一筆 `FEED_RECOVERED`（含來源與持續時間）；既有事件 SHALL NOT 被更新。

#### Scenario: 連續失敗只記一筆

- **WHEN** 某來源連續 10 次輪詢都失敗後恢復
- **THEN** `events` 中新增 1 筆 `FETCH_ERROR` 與 1 筆 `FEED_RECOVERED`，而不是 10 筆

#### Scenario: 錯誤類型改變視為新的轉換

- **WHEN** 某來源先因 `Timeout` 失敗，之後改為因 `RateLimited` 失敗
- **THEN** 各寫入一筆對應類型的事件
