## Why

Python 版把狀態散在 6 種 JSON/JSONL 檔裡，沒有任何檔案鎖：兩個 tab 同時存設定會互相覆蓋（HANDOFF Fragility #3）、
`has_symbol_pending()` 加 `add()` 不是原子操作（#2）、kill switch 讀到壞檔會回傳「沒停機」、
佇列檔損毀會從空狀態起步並覆蓋原檔、`events.jsonl` 的清理用非原子的 `write_text`，崩潰時會截斷「永久保存」的紀錄。
SQLite 以 transaction、trigger 與單一檔案一次解決這幾類問題，並讓之後的策略使用紀錄可以用 SQL 查詢。

## What Changes

- 在 `app` 內新增 `store` 模組（SQLite，`rusqlite`）：events、pairs、order_intents、config、system_flags、portfolio_history。
- `events` 只能新增：以 `BEFORE UPDATE` / `BEFORE DELETE` trigger 在資料庫層阻擋。
- **失敗即封閉**：資料庫無法開啟、migration 失敗、schema 版本比程式新、讀取設定或 kill switch 失敗，一律進入「停機」狀態並拒絕寫入，不得以空狀態繼續。
- 送單意圖先落地：`order_intents` 以 `client_order_id` 為唯一鍵，供之後「送單前先持久化、重啟後對帳」使用。
- 原子的「同標的只允許一個待處理配對」：以資料庫唯一索引保證，取代 Python 版的先檢查再寫入。
- API key 存 macOS Keychain；資料庫檔案權限 0600；日誌寫入前遮蔽簽名與金鑰。
- 唯讀、可重跑的 `events.jsonl` 匯入器：匯入前先驗證完整性，原檔不動。
- `SCAN_RUN` 事件不進永久事件表，只留記憶體環狀緩衝供日誌頁顯示最近紀錄（取代 Python 版每天清除一次的機制）。

## Capabilities

### New Capabilities
- `event-store`: 只能新增的事件表、事件欄位、`SCAN_RUN` 的處理方式。
- `durable-state`: 設定、kill switch、配對、送單意圖、資產歷史的持久化，以及失敗即封閉與原子操作。
- `secret-storage`: Keychain 金鑰存取、資料庫檔案權限、日誌遮蔽。
- `legacy-event-import`: 舊 `events.jsonl` 的完整性驗證與一次性、可重跑、唯讀的匯入。

### Modified Capabilities
<!-- 無 -->

## Impact

- `app` 新增依賴：`rusqlite`（bundled SQLite）、macOS Keychain 存取 crate、`sha2`（匯入器雜湊）。
- 新增資料庫檔：`~/Library/Application Support/tong-funding/funding.db`（路徑於 design.md 說明）。
- 依賴 change `core-domain-and-fixtures` 的型別（Pair 狀態、風控設定）。
- **不修改** `mvp-python/data/` 下任何檔案。
