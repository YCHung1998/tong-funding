## ADDED Requirements

### Requirement: 成交確認依訂單而非依持倉

成交確認 SHALL 以交易所 order id（必要時以 `client_order_id`）查詢該筆訂單的狀態與累計成交量，SHALL NOT 以「該標的是否存在持倉」或容差比例判定成交。
每一腿的狀態 SHALL 為已完整成交、部分成交（附成交量）、未成交、結果未知之一。

#### Scenario: 同標的既有持倉不能代替成交

- **WHEN** 交易所上該標的另有 1.0 的持倉，但本單累計成交量為 0
- **THEN** 該腿狀態為未成交

#### Scenario: 部分成交

- **WHEN** 某腿請求 0.020、累計成交 0.014
- **THEN** 該腿狀態為部分成交，成交量為 0.014

#### Scenario: 查單失敗

- **WHEN** 查單請求逾時
- **THEN** 該腿狀態為結果未知，而不是未成交

### Requirement: 不平衡以實際成交數量計算

兩腿都有成交後，系統 SHALL 以兩腿累計成交的幣量計算不平衡百分比 `|L − S| ÷ max(L, S) × 100`，並與該配對生效的 `max_leg_imbalance_pct`（雙腿取小）比較。
不平衡大於生效門檻時 SHALL 轉為 `IMBALANCED`；在門檻內時 SHALL 轉為 `RECONCILED`。
不平衡值與兩腿成交量 SHALL 寫入事件。

#### Scenario: 在容許內

- **WHEN** long 成交 0.0200、short 成交 0.0199、生效 `max_leg_imbalance_pct` 為 1.0
- **THEN** 不平衡約 0.5%，配對為 `RECONCILED`

#### Scenario: 超過容許

- **WHEN** long 成交 0.0200、short 成交 0.0190、生效 `max_leg_imbalance_pct` 為 1.0
- **THEN** 不平衡為 5%，配對為 `IMBALANCED` 並觸發警示

### Requirement: 逾時使用生效的 order_timeout_seconds 且只撤銷自己未成交的訂單

成交等待上限 SHALL 為該配對生效的 `order_timeout_seconds`（雙腿取小），自兩腿中最早的 `request_sent_at` 起算；系統 SHALL NOT 以固定常數取代該設定。
逾時時，系統 SHALL 只撤銷 `order_intents` 中屬於本配對、且尚未完整成交的訂單，SHALL NOT 動用任何持倉。
撤單後 SHALL 再查詢一次各腿最終成交量，並以最終成交量呼叫 core 的 `next()`；撤單失敗或最終狀態無法確認 SHALL 轉為 `UNRESOLVED`。

#### Scenario: 逾時前已成交

- **WHEN** 兩腿都在 `order_timeout_seconds` 內完整成交
- **THEN** 不發出任何撤單請求

#### Scenario: 撤單與成交競爭

- **WHEN** 逾時撤單時交易所回覆「訂單已成交」之類的錯誤，之後查單顯示該腿已完整成交
- **THEN** 該腿以完整成交計，最終狀態依兩腿最終成交量決定

#### Scenario: 撤單結果無法確認

- **WHEN** 逾時撤單的請求本身逾時、之後查單也失敗
- **THEN** 配對為 `UNRESOLVED` 並觸發警示

### Requirement: 逾時處理不產生任何自動補單

逾時與部分成交的處理 SHALL NOT 產生任何新的開倉、補買、補賣或平倉訂單；唯一允許的自動動作是撤銷自己送出且未成交的訂單。

#### Scenario: 一腿完成、另一腿 70%

- **WHEN** 逾時時 long 已 100%、short 僅 70%
- **THEN** 撤銷 short 腿剩餘未成交部分，配對為 `PARTIAL_FAILURE`，且送單請求的總數仍為 2

### Requirement: 成交等待以輪詢進行並遵守限流

成交等待 SHALL 以固定間隔輪詢各腿查單，直到兩腿皆達終局狀態或逾時；輪詢請求 SHALL 與其他請求共用同一個限流器。
收到限流回應時，輪詢 SHALL 依 `Retry-After` 退避，且 SHALL NOT 因此把該腿判為失敗。

#### Scenario: 輪詢中被限流

- **WHEN** 輪詢期間收到 HTTP 429
- **THEN** 輪詢暫停至 `Retry-After` 後繼續，該腿狀態不變
