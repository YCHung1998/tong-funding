## ADDED Requirements

### Requirement: 每筆送單先落地意圖，寫入成功才呼叫交易所

引擎 SHALL 在每一次送單（進場、出場、手動）之前，以唯一的 `client_order_id` 寫入 `order_intents`（狀態為已意圖）；寫入成功後才可呼叫 `Executor`。
`client_order_id` SHALL 由配對、腿、動作（開倉或平倉）與序號決定性地產生，SHALL 只含英數字、底線與連字號，長度 SHALL 不超過 36 個字元。
意圖寫入失敗時，引擎 SHALL NOT 呼叫 `Executor`，並進入停機。

#### Scenario: 意圖先於呼叫

- **WHEN** 引擎準備送出某腿訂單，並在 `Executor` 的送單函式入口記錄當時 store 內容
- **THEN** 此時 `order_intents` 已有該 `client_order_id`（先寫已意圖、再標已送出，兩者都在呼叫之前完成）

#### Scenario: 意圖寫入失敗

- **WHEN** 寫入 `order_intents` 時 store 回傳錯誤
- **THEN** `Executor` 的送單函式被呼叫 0 次，引擎進入停機

#### Scenario: client_order_id 的格式限制

- **WHEN** 為任一配對、腿、動作與序號產生 `client_order_id`
- **THEN** 結果長度不超過 36、只含 `[A-Za-z0-9_-]`，且相同輸入得到相同輸出

### Requirement: 送單結果未知時不得換新 id 重送

送單呼叫的結果為「未知」（逾時、斷線、回應無法解析）時，意圖 SHALL 維持「已送出、結果未知」，SHALL NOT 以新的 `client_order_id` 重送同一筆訂單，SHALL NOT 標為失敗。
引擎 SHALL 以 `client_order_id` 向交易所查詢該意圖的實際結果，並依查詢結果更新意圖。

#### Scenario: 逾時後以原 id 查詢

- **WHEN** 某腿送單逾時（結果未知）
- **THEN** 該意圖狀態為「已送出、結果未知」，引擎以同一 `client_order_id` 查詢，且 `Executor` 的送單函式沒有再被呼叫

### Requirement: 重啟後列出未結束意圖並與交易所對帳

程式啟動時，引擎 SHALL 從 store 列出所有未結束的意圖，並以 `client_order_id` 查詢每一筆的實際狀態，再與交易所回報的持倉與未成交委託比對。
對帳 SHALL 為唯讀：SHALL NOT 在對帳過程中送出任何新訂單，也 SHALL NOT 自動撤單或平倉。
對帳結果 SHALL 依下列規則轉移配對狀態，並寫入事件：
所有意圖皆確認完整成交且持倉吻合者，回到對應的正常狀態；
一腿確認成交、另一腿確認失敗或未成交者，為 `PARTIAL_FAILURE`；
查不到意圖、或意圖與持倉/委託對不上者，為 `UNRESOLVED`；
兩腿皆確認未成交且持倉與委託皆無曝險者，為 `CANCELLED`。

#### Scenario: 在「已意圖」時終止

- **WHEN** 程式在某腿意圖為已意圖、尚未呼叫交易所時被終止，重啟後交易所查無該 `client_order_id`、且該標的無持倉無委託
- **THEN** 該意圖被標為未送達、配對為 `CANCELLED`，且對帳過程沒有任何下單呼叫

#### Scenario: 在「已送出」時終止，訂單實際已成交

- **WHEN** 程式在某腿意圖為已送出時被終止，重啟後交易所顯示該訂單已完整成交，另一腿也已成交且持倉吻合
- **THEN** 意圖更新為已成交，配對回到 `FILL_MONITOR` 之後的正常流程，且不重複送單

#### Scenario: 一腿成交、另一腿查無

- **WHEN** 重啟後 long 腿確認成交、short 腿查無意圖也查無持倉
- **THEN** 配對為 `PARTIAL_FAILURE` 並觸發警示，long 腿資料被保留

#### Scenario: 持倉對不上

- **WHEN** 重啟後兩腿意圖皆顯示成交，但交易所持倉數量與意圖不符
- **THEN** 配對為 `UNRESOLVED` 並觸發警示

### Requirement: 對帳完成前不接受增加曝險的動作

啟動後，在所有未結束的 demo 意圖完成對帳之前，引擎 SHALL 在 `EXCHANGE_DEMO` 模式下拒絕所有 `opens_exposure()` 為真的 Command，排程器 SHALL NOT 觸發進場；`SIMULATION` 模式的進場不受 demo 對帳狀態影響（使用者 2026-10-05 決定），但未對帳的 demo 配對仍計入已開啟配對。
對帳因交易所不可連線、金鑰不可用或查詢失敗而無法完成時，引擎 SHALL 維持此狀態並顯示原因，SHALL NOT 因無法對帳而放行。
沒有任何未結束意圖時，此限制 SHALL 立即解除。

#### Scenario: 交易所不可連線

- **WHEN** 重啟後存在未結束意圖，但向交易所查詢全部失敗
- **THEN** 引擎維持「對帳未完成」狀態，進場 Command 被拒絕，介面顯示原因

#### Scenario: 沒有未結束意圖

- **WHEN** 重啟時 `order_intents` 沒有任何未結束的項目
- **THEN** 引擎直接進入可運作狀態

### Requirement: SIMULATION 中斷的配對轉入 UNRESOLVED

重啟時，若存在 `sim` 前綴的未結束意圖或處於進行中狀態的模擬配對，引擎 SHALL 不向交易所查詢這些意圖；因模擬持倉帳不持久化，這些配對 SHALL 轉為 `UNRESOLVED` 並記錄「模擬中斷」事件，等待人工事件離開。

#### Scenario: 模擬中斷

- **WHEN** 模擬進行到 `FILL_MONITOR` 時程式被終止並重啟
- **THEN** 該配對為 `UNRESOLVED`，事件註明模擬中斷，且沒有任何交易所請求被送出

### Requirement: 崩潰測試涵蓋兩個關鍵時點

引擎 SHALL 提供僅供測試使用的故障注入點，使程式能在「意圖已寫入、尚未呼叫 `Executor`」與「`Executor` 已被呼叫、結果尚未寫回」兩個時點終止，並以同一資料庫檔重啟後驗證上述對帳規則。

#### Scenario: 兩個時點都有測試

- **WHEN** 執行崩潰測試套件
- **THEN** 兩個終止時點各至少有一個測試，且每個測試重啟後都斷言：未重複下單、狀態符合對帳規則、警示（如適用）已觸發
