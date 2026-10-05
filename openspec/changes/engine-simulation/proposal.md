## Why

Python 版把交易流程放在 Streamlit 背景執行緒與 `st.cache_resource` 單例裡，造成 HANDOFF 列出的競態與不可測試的時間依賴。
這個 change 建立 `engine`：單一擁有者的 actor，把排程、Node 0–8 與 kill switch 在 **SIMULATION** 下完整跑通，
並把抗辯確認的缺口（送單前持久化意圖、重啟對帳、過期價格阻擋、時鐘注入）做進去。**本 change 不呼叫任何下單端點，也不包含任何能下單的程式碼。**

## What Changes

- `engine` 模組：以單一 actor 擁有全部可變狀態，UI 只送 Command、只收限頻的 Snapshot；下單與其他 I/O 由 spawn 的 task 執行，結果以內部 Event 回送；行情走獨立 `watch`。
- 排程器：1 秒 tick、時鐘由外部注入；進場為結算前 15 秒、出場為結算後 15 秒，結算時間以 `serverTime` 偏移校正；錯過進場視窗不補進場。
- Node 0–8 流程，搭配 `SimulatedExecutor`：該實作**沒有 client 欄位**，SIMULATION 下結構上不可能下單；SIMULATION 完整走完進場至出場，不再於進場後直接 `FINALIZED`（Python 版行為）。
- `opens_exposure()` 窮舉判斷每個 Command 是否增加曝險；kill switch 只攔會增加曝險者，且不強制平倉。
- `trigger_mode`（AUTO / MANUAL）與 `execution_mode`（SIMULATION / EXCHANGE_DEMO）是兩個獨立開關；全系統只有一條下單路徑（手動下單頁也經過它）。
- 風控覆寫（`effective_for_pair` 雙腿保守值）接進 Node 0 與所有下單前判斷，包含實際讀取 `order_timeout_seconds`。
- 重啟恢復：送單前先落地意圖（`client_order_id`）、重啟列出未結束意圖並與交易所對帳（唯讀），對不上就進入 `UNRESOLVED` 或 `PARTIAL_FAILURE` 並警示；對帳完成前不接受增加曝險的動作。
- PREPARED 配對在條件惡化時自動撤銷（只移除曝險，即使停機也執行）。

## Capabilities

### New Capabilities
- `engine-core`: actor、Command / Event / Snapshot、`opens_exposure` 窮舉分類、I/O 不阻塞、先落地再生效、限頻推送。
- `scheduler-and-nodes`: 進出場排程、Node 0–8、時鐘注入與校時、基準價分離、逾時分流、PREPARED 自動撤銷。
- `execution-modes`: 兩個獨立開關、單一下單路徑、`SIMULATION` 結構保證、kill switch、可腳本化的 `SimulatedExecutor`。
- `crash-recovery`: 送單前落地的意圖、`client_order_id` 規則、重啟對帳、fail-closed 的啟動流程。

### Modified Capabilities
<!-- 無 -->

## Impact

- 依賴 `core-domain-and-fixtures`、`store-sqlite`、`exchange-readonly-adapters`。
- 手動下單頁（`ui-trading-pages`）也經由同一條下單路徑，所以也受 `execution_mode` 約束。
- 本 change 定義 `Executor` 與 `AccountView` 介面；真實實作在 `exchange-demo-execution`。
- **前置條件（需在實作前確認，見 design.md Open Questions）**：
  - `core` 的 `next()` 需包含本 change 使用的轉移：`PREPARED → CANCELLED`、進行中狀態（`ORDER_SUBMIT`、`FILL_MONITOR`、`CLOSING`）因對帳結果轉入 `UNRESOLVED` / `PARTIAL_FAILURE`。`core-domain-and-fixtures` 的 spec 目前沒有明列這些轉移。
  - 向交易所以 `client_order_id` 查單的能力不在 `exchange-readonly-adapters` 的範圍內，由 `exchange-demo-execution` 的 `Executor` 提供；`SimulatedExecutor` 以模擬帳實作。
