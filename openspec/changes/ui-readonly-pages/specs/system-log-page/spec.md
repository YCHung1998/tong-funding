## ADDED Requirements

### Requirement: 事件時間軸按時間由新到舊顯示

系統日誌頁 SHALL 以時間軸列出事件，最新的在最上方，每列顯示：時間（UTC，精確到毫秒，並附日期）、事件類型標籤、詳情（結構化 JSON）。
頁首 SHALL 顯示符合目前篩選的事件總筆數與時間範圍（「最早 — 最新 UTC」）。
頁面 SHALL 為唯讀：SHALL NOT 提供編輯、刪除或清除事件的任何控制項。
頁面 SHALL 以分頁或增量載入顯示事件，SHALL NOT 一次把整個 `events` 表載入畫面清單；每次載入的筆數上限由 design.md 決定。

#### Scenario: 排序與格式

- **WHEN** 有兩筆事件，時間為 07:42:16.000 與 07:41:58.000（同一天）
- **THEN** 07:42:16.000 的事件在上方，每列顯示毫秒與日期

#### Scenario: 沒有任何寫入控制項

- **WHEN** 檢視系統日誌頁的所有控制項
- **THEN** 只有篩選與載入更早事件的控制項，沒有編輯、刪除或清除

#### Scenario: 事件很多時只載入一頁

- **WHEN** `events` 表有 7,000 筆以上
- **THEN** 頁面首次只載入最新的一頁，並提供載入更早事件的方式，頁首的總筆數仍為符合篩選的全部筆數

### Requirement: 事件來源為永久事件表加記憶體的 SCAN_RUN 緩衝

時間軸 SHALL 合併兩個來源：`events` 表（永久、不可變）與記憶體中的 `SCAN_RUN` 環狀緩衝，並依時間排序。
來自記憶體緩衝的 `SCAN_RUN` 列 SHALL 帶有「僅本次運行」標示；頁面 SHALL 在 `SCAN_RUN` 篩選選項旁說明「重啟後不保留」。
頁面 SHALL NOT 暗示 `SCAN_RUN` 是完整歷史；緩衝已滿並丟棄舊筆時，頁面 SHALL 能顯示目前緩衝涵蓋的起始時間。
從舊 `events.jsonl` 匯入的事件 SHALL 與新事件一同顯示，並帶有「匯入」標示（依 `legacy_hash` 是否存在判定）。

#### Scenario: SCAN_RUN 的標示

- **WHEN** 時間軸中有來自記憶體緩衝的 `SCAN_RUN`
- **THEN** 該列帶有「僅本次運行」標示

#### Scenario: 重啟後 SCAN_RUN 消失

- **WHEN** 程式重啟
- **THEN** 時間軸中沒有重啟前的 `SCAN_RUN`，但 `events` 表中的事件都還在

#### Scenario: 匯入事件的標示

- **WHEN** 某事件的 `legacy_hash` 不為空
- **THEN** 該列帶有「匯入」標示

### Requirement: 事件類型多選篩選

頁面 SHALL 提供事件類型多選篩選器，選項 SHALL 取自資料中實際出現的類型（`events` 表的相異 `event_type`，加上緩衝中有 `SCAN_RUN` 時的 `SCAN_RUN`），SHALL NOT 使用寫死的類型清單。
預設 SHALL 全選；篩選後的事件總筆數 SHALL 隨之更新；頁尾 SHALL 顯示各類型的筆數（例如「ORDER_SUBMITTED 5 · SCAN_RUN 2 · FETCH_ERROR 1」）。
取消全部類型時，頁面 SHALL 顯示「未選擇任何事件類型」，而不是顯示全部。

#### Scenario: 篩選為單一類型

- **WHEN** 只勾選 `FETCH_ERROR`
- **THEN** 時間軸只剩 `FETCH_ERROR` 事件，頁首的筆數為其總數

#### Scenario: 新出現的類型自動成為選項

- **WHEN** `events` 表中出現先前沒有的 `FEED_RECOVERED` 類型
- **THEN** 篩選器的選項包含 `FEED_RECOVERED`，無需修改程式

#### Scenario: 全部取消

- **WHEN** 取消所有類型
- **THEN** 顯示「未選擇任何事件類型」

### Requirement: 詳情完整呈現事件 payload

每列的詳情 SHALL 以 JSON 呈現該事件完整的 `payload`，SHALL NOT 截斷欄位、重新命名欄位或省略欄位；過長時 SHALL 換行或可展開，不得被裁切。
`payload` 已在寫入前經遮蔽，頁面 SHALL 原樣顯示，SHALL NOT 嘗試還原被遮蔽的值。
`payload` 不是合法 JSON 的列（不應出現，因 `events.payload` 有合法性檢查）SHALL 以原文顯示並標示「格式錯誤」，不得使頁面失敗。

#### Scenario: 完整欄位

- **WHEN** 某 `ORDER_SUBMITTED` 事件的 payload 有 12 個欄位
- **THEN** 詳情中的 JSON 包含這 12 個欄位與原值

#### Scenario: 遮蔽值原樣顯示

- **WHEN** payload 中某欄位的值為遮蔽用的占位文字
- **THEN** 詳情中顯示該占位文字

### Requirement: 健康狀態事件的恢復要以獨立事件呈現

資料來源失敗與恢復 SHALL 以兩筆獨立事件顯示（`FETCH_ERROR` 與之後的 `FEED_RECOVERED`），因為已寫入的事件不可被更新。
頁面 SHALL 不得合併兩筆事件、也不得更改既有事件的內容來表示「已恢復」；頁尾計數 SHALL 以各自的類型計算。

#### Scenario: 失敗後恢復

- **WHEN** Bybit 先失敗後恢復
- **THEN** 時間軸有一筆 `FETCH_ERROR` 與較晚的一筆 `FEED_RECOVERED`，兩筆獨立顯示
