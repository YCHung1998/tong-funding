## ADDED Requirements

### Requirement: 事件表只能新增

`events` 表 SHALL 只允許 INSERT。資料庫層 SHALL 以 trigger 阻擋對該表的 UPDATE 與 DELETE，使任何應用程式碼（包含未來的錯誤程式碼）都無法改寫或刪除既有事件。
每筆事件 SHALL 包含：自動遞增的 `id`、毫秒時間戳 `ts_ms`、`event_type`、可為空的 `pair_id`、合法 JSON 的 `payload`。
`payload` SHALL 保留該事件類型的完整欄位，不因欄位多寡而截斷。

#### Scenario: 無法更新事件

- **WHEN** 對已存在的事件執行 UPDATE
- **THEN** 資料庫拒絕該操作並回傳錯誤，原資料不變

#### Scenario: 無法刪除事件

- **WHEN** 對 `events` 表執行 DELETE（含不帶條件的全表刪除）
- **THEN** 資料庫拒絕該操作並回傳錯誤，筆數不變

#### Scenario: payload 必須是合法 JSON

- **WHEN** 嘗試寫入 `payload` 不是合法 JSON 的事件
- **THEN** 寫入被拒絕

### Requirement: SCAN_RUN 不寫入永久事件表

`SCAN_RUN`（每次掃描的摘要）SHALL NOT 寫入 `events` 表，以維持「事件表沒有任何例外、永不清除」的規則。
系統 SHALL 在記憶體中保留最近的 `SCAN_RUN`（上限筆數由 design.md 決定）供系統日誌頁顯示，程式重啟後可以消失。
所有其他事件類型 SHALL 寫入 `events` 表且永久保存。

#### Scenario: 掃描事件不進資料庫

- **WHEN** 系統完成一次掃描並產生 `SCAN_RUN`
- **THEN** `events` 表的筆數不變，而記憶體緩衝中多一筆

#### Scenario: 緩衝有上限

- **WHEN** 緩衝已達上限後再產生新的 `SCAN_RUN`
- **THEN** 最舊的一筆被丟棄，最新的被保留

#### Scenario: 交易事件永久保存

- **WHEN** 系統產生 `ORDER_SUBMITTED` 事件
- **THEN** 事件被寫入 `events` 表，之後無法被修改或刪除
