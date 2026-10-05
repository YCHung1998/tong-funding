## Context

來源：HANDOFF.md 的 Fragilities #2、#3、red-team 對 `kill_switch.py`、`order_queue_store.py`、`logger.py` 的指出（已對照原始碼確認）。

**舊事件檔的實際狀況**（唯讀檢視 `mvp-python/data/events.jsonl`，2026-10-05 快照）

| 項目 | 數值 |
|---|---|
| 檔案大小 / 總行數 | 1.1 MB / 7,129 行 |
| 不合格行 | 0（皆為合法 JSON，皆含 `ts` 與 `event_type`，以換行結尾） |
| 含 `pair_id` 的行 | 1,060 |
| 最多的類型 | `SCAN_RUN` 4,844、`FETCH_ERROR` 1,168、`ORDER_SUBMITTED` 473 |
| 其餘 | `PAIR_RECONCILIATION` 160、`ORDER_STAGED` 93、`PAIR_PARTIAL_FAILURE` 47、`PAIR_FINALIZED` 48 等 |
| SHA-256（當下） | `689da762ce0d5c146bf136815ef9742f085d1cd08d2b698d8d08bafeda76a6f5` |

按規則匯入預期為 7,129 − 4,844 = **2,285** 筆（檔案仍在成長，實際數字以執行當下為準）。
`PAIR_PARTIAL_FAILURE` 有 47 筆、`PAIR_FINALIZED` 有 48 筆，單腿失敗在 Python 版的 demo 運行中非常頻繁（原因未查證），
這會直接影響「單腿失敗完全人工處理」的警示頻率，列入 `exchange-demo-execution` 時再評估。

## Goals / Non-Goals

**Goals**
- 消除「檔案無鎖、讀壞檔即 fail-open、非原子清理」這三類已確認的缺陷。
- 事件不可改寫，並讓策略使用紀錄可用 SQL 查詢。
- 舊事件完整保留且不動原檔。

**Non-Goals**
- 不做 hash chain 與異地備份（見 Open Questions）。
- 不做多程序同時開啟的支援（單人工具，開啟時以檔案鎖拒絕第二個實例）。
- 不遷移其他舊資料檔（`order_queue_store.json` 等是 demo 狀態，沒有保留價值）。

## Decisions

**D1　SQLite（`rusqlite` bundled），WAL 模式，`foreign_keys = ON`，`busy_timeout` 設定。**
單檔、可交易、可查詢。使用者在 2026-10-05 選擇。

**D2　資料庫路徑：`~/Library/Application Support/tong-funding/funding.db`。**
macOS 慣例的應用資料目錄；不放在專案目錄，避免被 git 追蹤、避免與原始碼混在一起。

**D3　事件不可變由 trigger 保證，不另寫應用層機制。**
`BEFORE UPDATE`、`BEFORE DELETE` 以 `RAISE(ABORT, ...)` 阻擋。simplifier 與 red-team 都認為 trigger 已足夠；
SQLite 檔本身仍可被外部工具改寫，這個風險以 Open Questions 記錄，不在 v1 解決。

**D4　`SCAN_RUN` 不入庫，記憶體環狀緩衝保留最近 N 筆。**
N 暫定 200（我的提議）。好處：「事件表無例外、永不清除」成立，不需要 TTL 表與清理程式。
代價：重啟後日誌頁看不到先前的掃描摘要；Figma 日誌頁的 `SCAN_RUN` 標籤仍可用，只是只涵蓋本次運行。

**D5　失敗即封閉。**
Python 版 `kill_switch.is_halted()` 在 `JSONDecodeError` 時回 `False`；`OrderQueueStore._load` 讀到壞檔回空並在下次 persist 時覆蓋原檔。
兩者都是 fail-open。新版任何讀取失敗都進入停機並拒絕寫入。

**D6　原子新增靠唯一索引，不靠鎖。**
`pairs` 表對 `(symbol) WHERE status = 'PREPARED'` 建部分唯一索引；新增以 `INSERT ... ON CONFLICT DO NOTHING` 並檢查受影響列數。

**D7　`order_intents` 是「送單前落地」的基礎，但本 change 只提供表與存取函式。**
實際的送單流程與重啟對帳在 `engine-simulation` 與 `exchange-demo-execution`。

**D8　匯入器放在 `app` 內，以子命令呼叫，不另開 crate。**
與 `core + app` 兩 crate 的結論一致。

## Schema 草案

```
events(id INTEGER PK AUTOINCREMENT, ts_ms INTEGER NOT NULL, event_type TEXT NOT NULL,
       pair_id TEXT, payload TEXT NOT NULL CHECK(json_valid(payload)), legacy_hash TEXT UNIQUE)
  + trigger events_no_update, events_no_delete
pairs(internal_uuid TEXT PK, pair_id TEXT NOT NULL, symbol TEXT NOT NULL, status TEXT NOT NULL,
      entry_json TEXT NOT NULL, created_ms INTEGER, updated_ms INTEGER)
  + UNIQUE INDEX (symbol) WHERE status = 'PREPARED'
order_intents(client_order_id TEXT PK, pair_uuid TEXT, leg TEXT, exchange TEXT, symbol TEXT,
              side TEXT, quantity TEXT, state TEXT, exchange_order_id TEXT, created_ms, updated_ms)
config(key TEXT PK, value_json TEXT NOT NULL, version INTEGER NOT NULL, updated_ms INTEGER)
system_flags(key TEXT PK, value TEXT NOT NULL, updated_ms INTEGER)
portfolio_history(ts_ms INTEGER, exchange TEXT, total_usdt TEXT, PRIMARY KEY(ts_ms, exchange))
schema_version(version INTEGER)
```

## Risks / Trade-offs

- **SQLite 檔可被外部工具改寫，trigger 擋不住。** 對單人本機工具屬可接受風險；若之後需要可稽核性，再開 hash chain change。
- **macOS Keychain 的存取在未簽名的開發版 binary 上，可能每次都跳授權提示。** 這是行為層面的不便，不是安全問題；需實機確認。
- **`SCAN_RUN` 重啟即失。** 這是刻意取捨；若使用者想事後分析掃描歷史，需要改設計。
- **匯入器假設舊檔的 `ts` 是 Unix 秒。** 已對 7,129 行確認皆為 1.79e9 量級，符合；但仍以驗證階段檢查數值範圍。

## Open Questions

- 是否需要週期性備份資料庫（例如 WAL checkpoint 後複製）與 hash chain？目前列為 Non-Goal，red-team 建議過。
- `SCAN_RUN` 緩衝上限 200 是否合適？
- Keychain 在開發版 binary 的授權提示行為，task 8 實機驗證後記錄。
