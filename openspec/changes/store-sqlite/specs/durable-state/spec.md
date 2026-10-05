## ADDED Requirements

### Requirement: 失敗即封閉

下列任一情況發生時，系統 SHALL 進入「停機」狀態，並 SHALL 拒絕所有寫入與所有會增加曝險的動作；SHALL NOT 以空狀態或預設值繼續運作：
資料庫檔案無法開啟、檔案損毀、migration 失敗、資料庫 schema 版本高於程式所知的最高版本、讀取設定失敗、讀取 kill switch 狀態失敗。
停機的原因 SHALL 顯示在使用者介面，且 SHALL NOT 因此刪除或覆蓋任何既有資料檔。

#### Scenario: 損毀的資料庫

- **WHEN** 資料庫檔案被截斷或內容不是有效的 SQLite
- **THEN** 程式啟動進入停機狀態並顯示原因，原檔保持原樣未被覆蓋

#### Scenario: schema 版本比程式新

- **WHEN** 資料庫的 schema 版本高於程式支援的最高版本
- **THEN** 進入停機狀態，且不嘗試降版或修改 schema

#### Scenario: kill switch 讀取失敗視為停機

- **WHEN** 讀取 kill switch 狀態時發生錯誤
- **THEN** 系統視為「已停機」，而不是「未停機」

### Requirement: 設定與旗標持久化

風控設定、合約設定、各交易所覆寫、kill switch 狀態、`trigger_mode` 與 `execution_mode` SHALL 持久化於資料庫，並以 transaction 寫入。
兩個同時發生的寫入 SHALL NOT 造成其中一個的變更被靜默覆蓋為舊值；寫入以版本號或 transaction 序列化。
重啟後 SHALL 還原重啟前的值。

#### Scenario: 重啟後還原

- **WHEN** 使用者修改 `max_leverage` 並重啟程式
- **THEN** 重啟後該值仍為修改後的值

#### Scenario: 寫入失敗不留半套狀態

- **WHEN** 儲存設定的 transaction 中途失敗
- **THEN** 資料庫中的設定與儲存前完全相同

### Requirement: 同標的只允許一個待處理配對（原子）

系統 SHALL 以資料庫層的唯一性保證「同一標的最多只有一個 `PREPARED` 配對」。
「新增」SHALL 是單一原子操作：已存在則回報「已有待處理」，不得先查詢再寫入。

#### Scenario: 並行新增只成功一個

- **WHEN** 兩個執行緒同時對同一標的新增 `PREPARED` 配對
- **THEN** 恰好一個成功，另一個收到「已有待處理」，資料庫中只有一筆

#### Scenario: 已進入後續狀態後可再新增

- **WHEN** 該標的上一個配對已不是 `PREPARED`
- **THEN** 可以新增新的 `PREPARED` 配對

### Requirement: 送單意圖在送單前落地

每一腿的送單 SHALL 先以唯一的 `client_order_id` 寫入 `order_intents`（狀態為已意圖），寫入成功後才可呼叫交易所。
`client_order_id` SHALL 在整個資料庫中唯一；重複的 id SHALL 被拒絕。
意圖的狀態變化（已送出、已確認、已成交、已取消、失敗）SHALL 以 UPDATE 記錄於該表，並同時寫入一筆不可變的事件。

#### Scenario: 先落地才送單

- **WHEN** 引擎準備送出一腿訂單
- **THEN** 對應的意圖已存在於資料庫且狀態為已意圖，之後才進行交易所呼叫

#### Scenario: 重複的 client_order_id

- **WHEN** 嘗試以已存在的 `client_order_id` 建立意圖
- **THEN** 被拒絕，不產生第二筆

#### Scenario: 重啟後可找到未完成的意圖

- **WHEN** 程式在某筆意圖處於「已意圖」或「已送出」時被終止，之後重啟
- **THEN** 可以列出所有未結束的意圖，供引擎向交易所對帳

### Requirement: 資產歷史的 7 天保留

資產歷史 SHALL 存於獨立資料表，僅保留最近 7 天。清除 SHALL 在單一 transaction 中完成，且 SHALL NOT 影響 `events` 表。

#### Scenario: 清除舊資料

- **WHEN** 資料表中有 8 天前與 1 天前的資料，執行清除
- **THEN** 8 天前的資料被刪除，1 天前的資料保留，`events` 表筆數不變
