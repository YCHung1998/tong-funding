## ADDED Requirements

### Requirement: 進入警示狀態時三個通道各通知一次

配對每次進入 `PARTIAL_FAILURE`、`IMBALANCED` 或 `UNRESOLVED` 時，系統 SHALL 觸發三個通道：全頁常駐警示（以 Snapshot 提供）、macOS 系統通知、不可變事件記錄。
同一次進入 SHALL 只通知一次，重啟後 SHALL NOT 對已通知過的狀態重複發送系統通知。
事件 SHALL 含配對 id、原因分類、兩腿的交易所回報成交量、已成交那一腿的資料，且成功那一腿的資料 SHALL NOT 被丟棄。

#### Scenario: 單腿失敗

- **WHEN** 配對因 short 腿被拒單而進入 `PARTIAL_FAILURE`
- **THEN** Snapshot 含該配對的警示、通知器被呼叫一次、事件記錄含 long 腿成交資料

#### Scenario: 重啟後不重複通知

- **WHEN** 程式重啟，該配對仍處於 `PARTIAL_FAILURE`
- **THEN** Snapshot 仍含該警示，通知器沒有被再次呼叫

### Requirement: 警示由配對狀態推導，直到人工事件才消失

Snapshot 中的常駐警示 SHALL 由 store 中處於 `PARTIAL_FAILURE`、`IMBALANCED`、`UNRESOLVED` 的配對推導，SHALL NOT 只存在於記憶體；因此重啟後 SHALL 仍顯示。
警示 SHALL 只在配對因人工事件離開上述狀態時消失，SHALL NOT 因時間經過或系統事件而消失。
橫幅的繪製屬於 UI change；本 change SHALL 保證 Snapshot 的警示資料正確。

#### Scenario: 時間經過不會清除警示

- **WHEN** 配對處於 `PARTIAL_FAILURE`，假時鐘前進 24 小時
- **THEN** Snapshot 仍含該警示

#### Scenario: 重啟後仍顯示

- **WHEN** 程式在配對處於 `UNRESOLVED` 時重啟
- **THEN** 啟動後的第一份 Snapshot 就含該警示

### Requirement: 系統通知失敗不得影響警示

系統通知的發送失敗 SHALL NOT 阻塞引擎，也 SHALL NOT 取代其他通道；失敗時 SHALL 寫入一筆事件，且橫幅與事件記錄 SHALL 照常產生。

#### Scenario: 通知器回報失敗

- **WHEN** 發送 macOS 系統通知失敗
- **THEN** 寫入通知失敗事件，Snapshot 警示仍存在

### Requirement: 警示期間不自動動倉位

配對處於 `PARTIAL_FAILURE`、`IMBALANCED`、`UNRESOLVED` 期間，系統 SHALL NOT 自動送出任何訂單（開倉、補買、補賣、平倉）、SHALL NOT 自動撤單、SHALL NOT 自動重試；任何此類動作 SHALL 只由人工 Command 觸發。

#### Scenario: 警示持續一小時

- **WHEN** 配對處於 `PARTIAL_FAILURE`，假時鐘前進 1 小時，期間排程器持續 tick
- **THEN** `Executor` 的送單與撤單呼叫次數都沒有增加

#### Scenario: 警示配對仍佔用名額與阻擋同標的

- **WHEN** 某標的的配對處於 `PARTIAL_FAILURE`，之後對同一標的送單前檢查
- **THEN** `ExistingExposure` 失敗

### Requirement: 人工處理只有兩條出口且都須驗證

使用者 SHALL 只能以兩種人工 Command 使警示配對離開警示狀態：
「人工要求平倉」，轉為 `CLOSING` 並依 `pair-close` 執行；
「人工確認已平倉」，系統 SHALL 先向交易所重新查詢兩腿持倉與未成交委託，兩腿持倉皆為 0 且無未成交委託才接受，否則 SHALL 拒絕並回報當前持倉。
兩個 Command 的執行時間與結果 SHALL 寫入事件。

#### Scenario: 人工確認時仍有持倉

- **WHEN** 使用者在交易所端手動平倉後按下確認，但重新查詢發現 short 腿仍有持倉
- **THEN** Command 被拒絕並回報持倉，配對維持警示狀態

#### Scenario: 人工確認成功

- **WHEN** 重新查詢顯示兩腿持倉皆為 0 且無未成交委託
- **THEN** 配對離開警示狀態，警示消失，並寫入事件

### Requirement: 警示事件可依原因統計頻率

每筆警示事件 SHALL 帶有原因分類，至少包含：送單被拒、送單結果未知、成交逾時單腿、不平衡、重啟對帳不符、平倉單腿失敗、成交確認無法完成。
事件資料 SHALL 足以用資料庫查詢計算各原因在任一期間的次數。

#### Scenario: 依原因計數

- **WHEN** 對一段期間的事件依原因分類計數
- **THEN** 每筆警示事件恰好被歸入一個分類，且分類總數等於警示事件總數
