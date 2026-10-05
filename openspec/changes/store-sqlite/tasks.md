## 1. 資料庫基礎

- [x] 1.1 加入 `rusqlite`（bundled）；實作開啟資料庫（WAL、`foreign_keys`、`busy_timeout`）、路徑 `~/Library/Application Support/tong-funding/funding.db`、新建時先設 0600 再寫資料；測試主檔與 WAL/SHM 權限皆為 0600，權限過寬時修正並寫事件
- [x] 1.2 migration 框架與 schema v1（見 design.md 草案）；測試：全新資料庫建立成功；schema 版本高於程式時拒絕並進入停機

## 2. 不可變事件與失敗即封閉

- [x] 2.1 `events` 表與 UPDATE/DELETE trigger；測試先寫（UPDATE、DELETE、無條件 DELETE 皆被拒絕、非法 JSON 的 payload 被拒絕）並確認紅燈，再實作
- [x] 2.2 `SCAN_RUN` 記憶體環狀緩衝（上限 200）；測試：不進 `events`、超過上限丟最舊
- [x] 2.3 失敗即封閉：損毀資料庫、migration 失敗、讀設定失敗、讀 kill switch 失敗皆進入停機並拒絕寫入；測試用被截斷的資料庫檔，並證明原檔位元組未被改動

## 3. 持久化狀態

- [x] 3.1 設定、各交易所覆寫、kill switch、`trigger_mode`、`execution_mode` 的 transaction 寫入與版本號；測試：重啟後還原、transaction 中途失敗不留半套
- [x] 3.2 `pairs` 的原子新增（部分唯一索引）；測試：兩執行緒同時對同標的新增，恰好一個成功
- [x] 3.3 `order_intents`：`client_order_id` 唯一、狀態更新同時寫不可變事件、列出未結束意圖；資產歷史表與單一 transaction 的 7 天清除

## 4. 金鑰與遮蔽

- [ ] 4.1 金鑰存取介面與 macOS Keychain 實作、記憶體測試替身；讀取失敗視為未連線。實機驗證一次（存入、讀出、刪除）並記錄授權提示行為到 `design.md`
  - 進度（2026-10-05）：介面、Keychain 實作、記憶體替身與 exact-value registry 已完成並有測試；**實機驗證未做**（需使用者在 macOS 上執行，見 `TODO.md`），完成前不得封存
- [x] 4.2 日誌遮蔽函式（查詢參數與標頭）；測試涵蓋連線逾時錯誤字串內含簽名、標頭值；並掃描資料庫確認不含金鑰字串

## 5. 舊事件匯入

- [x] 5.1 匯入器的驗證階段（合法 JSON、含 `ts`/`event_type`、換行結尾）與報告；測試用被截斷與含壞行的複本，預設中止、明確略過時列出行號
- [ ] 5.2 匯入階段：欄位對應、略過 `SCAN_RUN`、`legacy_hash` 唯一、增量重跑、來源雜湊前後比對；對真實 `events.jsonl` 的**複本**實跑，貼出報告與前後 SHA-256，並確認「合格行數 = 匯入 + 略過 + 已存在」
  - 進度（2026-10-05）：匯入階段已完成並有測試；子命令已接上（D8）：`tong-funding import-legacy-events <events.jsonl> [--db <funding.db>] [--skip-invalid]`。**對真實檔案複本的實跑未做**（檔案只在使用者機器上，見 `TODO.md`），完成前不得封存
