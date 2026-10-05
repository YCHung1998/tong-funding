## ADDED Requirements

### Requirement: trigger_mode 與 execution_mode 是兩個獨立開關

`trigger_mode`（`AUTO` / `MANUAL`）與 `execution_mode`（`SIMULATION` / `EXCHANGE_DEMO`）SHALL 是兩個獨立開關，各自持久化、各自修改，修改其中一個 SHALL NOT 改變另一個。
`trigger_mode` 決定「誰來觸發進出場」，`execution_mode` 決定「觸發後訂單去哪裡」。
系統 SHALL NOT 使用「LIVE」作為任何模式名稱。

#### Scenario: AUTO 搭配 SIMULATION

- **WHEN** `trigger_mode` 為 `AUTO`、`execution_mode` 為 `SIMULATION`，進場時點到達
- **THEN** 排程器自動觸發進場，訂單送往 `SimulatedExecutor`

#### Scenario: MANUAL 不自動觸發

- **WHEN** `trigger_mode` 為 `MANUAL`，進場時點到達
- **THEN** 排程器不觸發進場，配對維持 `PREPARED` 等待人工 Command

#### Scenario: 修改一個不影響另一個

- **WHEN** 使用者把 `trigger_mode` 由 `MANUAL` 改為 `AUTO`
- **THEN** `execution_mode` 的值與持久化內容不變

### Requirement: 只有一條下單路徑，手動下單也受 execution_mode 約束

所有訂單（排程進場、排程出場、手動執行進場、手動下單頁、人工平倉）SHALL 經由同一個 `Executor` 介面送出，該介面的實作由 `execution_mode` 決定。
引擎 SHALL NOT 存在繞過 `Executor` 的第二條下單路徑；手動下單頁在 `SIMULATION` 下 SHALL 送往 `SimulatedExecutor`。

#### Scenario: SIMULATION 下的手動下單

- **WHEN** `execution_mode` 為 `SIMULATION`，使用者在手動下單頁送出一筆訂單
- **THEN** 該訂單由 `SimulatedExecutor` 處理並寫入模擬持倉，沒有任何交易所請求

### Requirement: SIMULATION 下的執行器在結構上不可能下單

`SimulatedExecutor` SHALL NOT 具有任何交易所 client 欄位，也 SHALL NOT 依賴任何能對交易所送出訂單的程式碼。
在 `SIMULATION` 期間，進程內 SHALL NOT 存在任何能下單的 `Executor` 實例：能下單的實作 SHALL 只由「切換至 `EXCHANGE_DEMO`」的工廠函式建立，且 SHALL 在切回 `SIMULATION` 時被丟棄。
此保證 SHALL 同時以依賴關係與測試證明。

#### Scenario: 整輪 SIMULATION 沒有建立下單 client

- **WHEN** 以注入的「真實執行器工廠」（內含呼叫計數）啟動引擎並跑完一整輪 `SIMULATION`
- **THEN** 工廠被呼叫 0 次，且測試用的網路攔截器記錄到 0 個對外下單請求

#### Scenario: 依賴關係證明

- **WHEN** 檢查 `SimulatedExecutor` 所在模組的 `use` 與依賴
- **THEN** 它不引用 `exchange` 模組中任何具備下單能力的型別（以測試掃描並於 CI 執行）

### Requirement: 模式切換只在所有已開啟配對都屬於目標模式時允許

切換 `execution_mode` 的 Command SHALL 在存在任何屬於另一模式（依配對建立時的模式）的已開啟配對（非 `FINALIZED`、`CANCELLED`、`BLOCKED`）時被拒絕並說明原因，避免模擬與真實的訂單混在同一配對；所有已開啟配對都屬於目標模式（或沒有已開啟配對）時 SHALL 允許。
切換至 `EXCHANGE_DEMO` 時，引擎 SHALL 先取得金鑰並建立真實執行器；任一步驟失敗 SHALL 維持 `SIMULATION` 並回報原因。
啟動時儲存的模式是 `EXCHANGE_DEMO` 但無法建立真實執行器時，引擎 SHALL 退回 `SIMULATION`、寫入退回事件，並在 `Snapshot` 中顯示給使用者的提醒，直到成功切回 `EXCHANGE_DEMO`。

#### Scenario: 有另一模式的配對時拒絕切換

- **WHEN** 存在一組模擬的 `RECONCILED` 配對，使用者要求切換至 `EXCHANGE_DEMO`
- **THEN** Command 被拒絕並回報原因，模式不變，且未建立真實執行器

#### Scenario: 退回後只剩 demo 配對時可以切回

- **WHEN** 啟動時因金鑰不可用退回 `SIMULATION`，留下一組 demo 配對；金鑰修好後使用者要求切換至 `EXCHANGE_DEMO`
- **THEN** 引擎先建立真實執行器，成功後切換為 `EXCHANGE_DEMO`，提醒消失

#### Scenario: demo 配對未結束時不能切回 SIMULATION

- **WHEN** 在 `EXCHANGE_DEMO` 下存在一組 demo 配對，使用者要求切換至 `SIMULATION`
- **THEN** Command 被拒絕並回報原因，模式不變

#### Scenario: 金鑰讀取失敗

- **WHEN** 要求切換至 `EXCHANGE_DEMO`，但讀取金鑰失敗
- **THEN** `execution_mode` 維持 `SIMULATION`，並回報「未連線」

#### Scenario: 啟動退回時提醒使用者

- **WHEN** 啟動時儲存的模式是 `EXCHANGE_DEMO`，但讀不到金鑰
- **THEN** `execution_mode` 為 `SIMULATION`，留下退回事件，且 `Snapshot` 含一則說明原因的提醒

### Requirement: kill switch 只攔增加曝險的 Command 且永不強制平倉

kill switch 停機時，引擎 SHALL 拒絕所有 `opens_exposure()` 為真的 Command（包含排程器內部的進場觸發），SHALL NOT 攔截 `opens_exposure()` 為假的 Command。
停機本身 SHALL NOT 觸發任何平倉或撤單。
讀取 kill switch 狀態失敗時 SHALL 視為已停機。

#### Scenario: 停機時進場被擋

- **WHEN** kill switch 已停機，進場時點到達
- **THEN** 進場不觸發，並記錄一筆被 kill switch 擋下的事件

#### Scenario: 停機時不會自行平倉

- **WHEN** kill switch 由未停機切為停機，且存在一組 `RECONCILED` 配對（出場時點未到）
- **THEN** 該配對狀態不變，`Executor` 沒有收到任何訂單

#### Scenario: 停機時人工平倉仍可執行

- **WHEN** kill switch 已停機，使用者對一組 `RECONCILED` 配對送出人工平倉 Command
- **THEN** 該 Command 被接受

#### Scenario: kill switch 狀態讀取失敗

- **WHEN** 讀取 kill switch 狀態時 store 回傳錯誤
- **THEN** 引擎視為已停機，並拒絕 `opens_exposure()` 為真的 Command

### Requirement: SimulatedExecutor 的行為可腳本化且不宣稱真實

`SimulatedExecutor` SHALL 以注入的價格來源與腳本決定每一筆訂單的結果（完整成交、部分成交、拒單、逾時不回應），使測試能重現單腿失敗、部分成交與未知結果。
預設行為 SHALL 為以注入的最新價格完整成交，且 SHALL 維護模擬的持倉與委託帳，供對帳與已平倉確認使用。
模擬結果 SHALL NOT 被當作交易所接受該請求的證據；所有模擬事件 SHALL 標記為模擬。

#### Scenario: 腳本化的單腿拒單

- **WHEN** 腳本設定 short 腿拒單、long 腿完整成交
- **THEN** 配對依 core 規則進入 `PARTIAL_FAILURE`，long 腿成交資料被保留

#### Scenario: 模擬事件可辨識

- **WHEN** 檢視 `SIMULATION` 產生的訂單事件
- **THEN** 每一筆都帶有模擬標記，且 `client_order_id` 帶有 `sim` 前綴
