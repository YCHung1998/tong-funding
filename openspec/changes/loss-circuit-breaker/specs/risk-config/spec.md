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

#### Scenario: 武裝中資料庫被改高仍以凍結值為準

- **WHEN** Live 已武裝（凍結值 5／2），資料庫裡的兩欄被繞過 App 改成 50／10
- **THEN** 熔斷以兩者中較嚴的凍結值 5／2 評估並觸發；武裝時的 `caps_hash` 與 `LIVE_ARMED` 事件都包含這兩個值

#### Scenario: Live 讀不到熔斷欄位

- **WHEN** Live 身分的設定缺少熔斷欄位（或整份設定讀不到、啟動補寫失敗）
- **THEN** 熔斷以 Live 預設 5／2 評估，不以缺欄位時載入的 15／3 評估

#### Scenario: 資料庫完全沒有風控設定列

- **WHEN** Live 資料庫沒有任何風控設定列（例如只有合約範本）
- **THEN** 啟動時一次性寫入完整的 Live 預設設定（`live_db_defaults().risk`：手動觸發、提前出場 0、熔斷 5／2 等，不只熔斷兩欄；`RISK_CONFIG_UPDATED`）；寫入失敗則寫 `LIVE_BREAKER_BACKFILL_FAILED`、維持鎖定並通知，不得吞掉

### Requirement: 設定命令列寫入前取得實例鎖

`tong-funding config set`（Demo 與 Live 皆然）SHALL 在開啟資料庫之前取得與 App 相同的單一實例鎖；App 執行中（鎖被佔用）或無法確定鎖的位置時 SHALL 拒絕寫入、不改任何資料，並說明要先關閉 App。`config show` 為唯讀：SHALL 以唯讀方式開啟資料庫，不寫入任何資料（包含 Live 預設值）、不取得單一實例鎖、不執行遷移，資料庫檔不存在時也 SHALL NOT 建立檔案，因此 App 執行中仍可執行。主資料庫檔不會被修改，但 SQLite 可能在資料庫旁建立 `-shm`／`-wal` 輔助檔，因此 SHALL 在 `config show` 說明中提醒請勿以 sudo 執行。

#### Scenario: App 執行中不能用命令列改設定

- **WHEN** App 正在執行，使用者執行 `tong-funding config set risk '...'`
- **THEN** 以非零碼結束並說明另一個實例已在執行、請先關閉；設定不變；`config show` 仍可執行

#### Scenario: 非法值

- **WHEN** 把 `loss_breaker_max_consecutive_losses` 設為 0
- **THEN** 驗證失敗並指出欄位名稱，設定不變
