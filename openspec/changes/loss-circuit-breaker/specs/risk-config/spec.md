## ADDED Requirements

### Requirement: 熔斷欄位

全域風控設定 SHALL 包含：

| 欄位 | 預設 | 驗證 |
|---|---|---|
| `loss_breaker_max_loss_usdt` | 15 | > 0 |
| `loss_breaker_max_consecutive_losses` | 3 | 整數 ≥ 1 |

新建立的 Live 資料庫 SHALL 以 5 與 2 為初始值。舊設定缺少這兩個欄位時 SHALL 以預設值載入，且 SHALL NOT 因此被標為「不完整」。兩者皆為「越低越嚴」，只有全域值、沒有每所覆寫。

#### Scenario: 舊設定載入

- **WHEN** 載入一份沒有熔斷欄位的既有設定
- **THEN** 兩欄分別為 15 與 3，設定完整性不受影響

#### Scenario: 既有 Live 資料庫的升級

- **WHEN** Live 身分啟動，資料庫裡已存的風控設定沒有熔斷欄位
- **THEN** 啟動時一次性把缺少的欄位補成 5 與 2（走既有的設定儲存路徑，寫 `RISK_CONFIG_UPDATED` 含 before／after）；已有值的欄位不動，Demo 不補

#### Scenario: Live 武裝期間只能調低

- **WHEN** Live 已武裝，使用者把熔斷兩欄之一調高
- **THEN** 被拒絕並說明該欄位武裝中不能調高（調低允許）

#### Scenario: 非法值

- **WHEN** 把 `loss_breaker_max_consecutive_losses` 設為 0
- **THEN** 驗證失敗並指出欄位名稱，設定不變
