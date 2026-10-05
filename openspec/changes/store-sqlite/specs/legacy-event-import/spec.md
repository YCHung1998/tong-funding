## ADDED Requirements

### Requirement: 匯入器對來源檔唯讀

匯入器 SHALL 以唯讀方式開啟舊的 `events.jsonl`，SHALL NOT 修改、搬移、重新命名、截斷或刪除它。
匯入前後來源檔的 SHA-256 SHALL 相同；若不同（例如另一個程序正在寫入），匯入器 SHALL 回報並中止，而不是匯入不確定的內容。

#### Scenario: 來源檔不被改動

- **WHEN** 對一份 `events.jsonl` 的複本執行匯入
- **THEN** 匯入後複本的 SHA-256 與匯入前相同

#### Scenario: 匯入期間來源被改動

- **WHEN** 匯入進行中來源檔的雜湊發生變化
- **THEN** 匯入器中止並回報，已寫入的部分以單一 transaction 回滾

### Requirement: 匯入前先驗證完整性

匯入器 SHALL 先掃描整個來源檔，驗證：每一行是合法 JSON、含 `ts`（數值）與 `event_type`（字串）、檔案以換行結尾（偵測最後一行被截斷）。
發現任何不合格的行，預設 SHALL 中止並輸出報告（行號與原因），不寫入任何資料。
只有使用者明確指定「略過不合格行」時，才可匯入其餘合格行，且被略過的行號與內容 SHALL 完整列在報告中。

#### Scenario: 全部合格

- **WHEN** 所有行皆合法
- **THEN** 驗證通過，進入匯入階段

#### Scenario: 發現被截斷的最後一行

- **WHEN** 來源檔最後一行不是完整 JSON，且檔案不以換行結尾
- **THEN** 預設中止，報告指出最後一行為不合格，資料庫未被寫入

#### Scenario: 明確略過不合格行

- **WHEN** 使用者指定略過不合格行，來源有 2 行不合格
- **THEN** 其餘合格行被匯入，報告列出那 2 行的行號與原始內容

### Requirement: 匯入規則與欄位對應

匯入器 SHALL 把每一行對應為一筆事件：`ts`（秒，浮點）轉為毫秒整數 `ts_ms`（四捨五入到毫秒）、`event_type` 原樣保留、
若原事件含 `pair_id` 則填入 `pair_id` 欄位，其餘所有欄位（含 `ts` 與 `event_type` 以外者）完整放入 `payload`。
不認得的 `event_type` SHALL 照樣匯入，不得丟棄。
`SCAN_RUN` SHALL 被略過不匯入（與 `event-store` 的規則一致），並計入報告的「略過」筆數。
匯入 SHALL 按來源檔的行序進行，並保持事件的相對順序。

#### Scenario: 欄位對應

- **WHEN** 匯入一行 `{"ts": 1791090102.635932, "event_type": "ORDER_STAGED", "pair_id": "p1", "symbol": "BTCUSDT"}`
- **THEN** 產生的事件 `ts_ms` 為 1791090102636、`event_type` 為 `ORDER_STAGED`、`pair_id` 為 `p1`，`payload` 含 `symbol`

#### Scenario: 未知事件類型也匯入

- **WHEN** 來源含一個本程式不認得的 `event_type`
- **THEN** 該事件被匯入，`payload` 完整保留

#### Scenario: SCAN_RUN 被略過

- **WHEN** 來源共 N 行，其中 K 行為 `SCAN_RUN`
- **THEN** 匯入 N − K 筆事件，報告顯示略過 K 筆

### Requirement: 匯入可重跑且不重複

每個匯入的事件 SHALL 記錄其來源行的 SHA-256（`legacy_hash`），並以唯一性約束防止重複匯入。
重跑匯入 SHALL 只新增尚未匯入的行，不改變已匯入的事件。
來源檔自上次匯入後新增了行時，重跑 SHALL 只匯入新增的行。

#### Scenario: 連續匯入兩次

- **WHEN** 對同一份來源連續執行匯入兩次
- **THEN** 第二次新增 0 筆，`events` 總筆數與第一次之後相同

#### Scenario: 來源新增行後增量匯入

- **WHEN** 第一次匯入後，來源檔又新增 3 行非 `SCAN_RUN` 事件，再次執行匯入
- **THEN** 只新增這 3 筆

### Requirement: 匯入結果報告

匯入完成 SHALL 輸出報告：來源總行數、合格行數、匯入筆數、略過筆數（依原因分類）、依 `event_type` 的筆數、時間範圍。
報告 SHALL 滿足「合格行數 = 匯入筆數 + 略過筆數 + 已存在筆數」。

#### Scenario: 報告數字自洽

- **WHEN** 匯入完成
- **THEN** 報告中的合格行數等於匯入筆數、略過筆數與已存在筆數之和
