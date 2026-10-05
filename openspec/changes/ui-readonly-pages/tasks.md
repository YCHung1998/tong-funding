測試指令：`cd /Users/eason.hung/Documents/github/tong-funding && cargo test -p tong-funding ui::`（view-model 測試與其原始碼同層，建議路徑 `app/src/ui/vm/<頁面>.rs` 與 `<頁面>_tests.rs`）。
標示「先紅後綠」的 task，要先寫測試並貼出失敗輸出（失敗原因須是預期的），再實作到通過。畫面繪製層以截圖對照 Figma 驗收，不計入「先紅後綠」。

## 1. 資料層到畫面的橋接

- [x] 1.1 定義 `ReadOnlyDataSource` 與 `UiSnapshot`（行情觀測、帳戶資料、持倉、健康狀態、設定、事件）；UI 只讀 snapshot；行情更新合併上限 2 Hz，倒數獨立 1 Hz。先紅後綠：1 秒內 5 次更新最多重算 2 次且最終資料完整；只有時間經過時不重算。驗收：`ui::bridge` 測試通過
  - 證據：`cargo test -p tong-funding ui::bridge`（6 passed；`app/src/ui/vm/bridge.rs` + `bridge_tests.rs`）。紅燈：拿掉限速時 `at most 2 recomputes in the second, got 5`。正式資料來源：`app/src/ui/live.rs`（`LiveSource`）。

## 2. 頁面

- [x] 2.1 總覽 view-model 與頁面：彙總卡、估值規則（不含名目、無法估值、未連線不計入）、占比與百分比位數、保證金分布（含估算標示）、曝險摘要、OKX 說明卡、輪詢失敗保留舊資料。先紅後綠：以 Figma 的 22,000 / 18,000 與 16,200 / 3,010 / 1,490 / 1,300 數字為測試案例。驗收：`ui::dashboard` 測試通過，截圖附上
  - 證據：`cargo test -p tong-funding ui::dashboard`（15 passed；`app/src/ui/vm/dashboard.rs` + `dashboard_tests.rs`）。紅燈：實作前 15 個皆 `not yet implemented: dashboard::…`。繪製：`app/src/ui/pages.rs`。**截圖待 4.1 在 Mac 上補**。
- [x] 2.2 持倉 view-model 與頁面：篩選（不影響配對與標題計數）、彙總卡、欄位與 Size 精度、「Funding 收到」為「—」、配對卡片與不平衡率、未配對標示、`Incomplete` 與未連線提示。先紅後綠；配對以測試用假配對驗證。驗收：`ui::positions` 測試通過，截圖附上
  - 證據：`cargo test -p tong-funding ui::positions`（13 passed；`app/src/ui/vm/positions.rs` + `positions_tests.rs`，假配對）。紅燈：12 個 `not yet implemented: positions::…`。**截圖待 4.1**。
- [x] 2.3 系統日誌 view-model 與頁面：事件表與 `SCAN_RUN` 緩衝合併排序、類型選項取自資料、篩選與計數、完整 JSON、「匯入」與「僅本次運行」標示、游標分頁、`FETCH_ERROR` 與 `FEED_RECOVERED` 獨立呈現。先紅後綠；以 7,000 筆的測試資料證明首次只載入一頁。驗收：`ui::system_log` 測試通過，截圖附上
  - 證據：`cargo test -p tong-funding ui::system_log`（12 passed；7,000 筆首頁只載 500 筆、總數仍 7,000）＋ `store::event_query`（3 passed，`app/src/store/event_query.rs`）。紅燈：12 個 `not yet implemented: system_log::…`。**截圖待 4.1**。
- [x] 2.4 掃幣 view-model：列集合（啟用的所與允許幣種）、週期標籤與 8h 等效、DATA_ERROR 與 NOT_LISTED 呈現、Gross Spread 與「週期不同」、Net Edge 與「未設定」（列出缺少項目）、達標欄與預設排序、OKX 僅比價（不參與方向與達標）、各列獨立倒數（校時、結算中）、toggle 與符合筆數、彙總卡一致性。先紅後綠：測試案例含 core 的 −0.04 手算例、週期 4h 對 8h、費率缺失、OKX 價差最大但不被選中。驗收：`ui::scanner` 測試通過
  - 證據：`cargo test -p tong-funding ui::scanner`（23 passed；`app/src/ui/vm/scanner.rs` + `scanner_tests.rs`）。紅燈：23 個 `not yet implemented: scanner::…`。
- [x] 2.5 掃幣頁繪製與「立即刷新」接線：Funding Rate Matrix（虛擬化）、頁首門檻唯讀與連結、來源狀態、toggle、刷新按鈕（進行中停用、重複點擊忽略、部分失敗、全部失敗）。先紅後綠：以假傳輸層證明刷新每個來源各發出新請求、`observed_at` 晚於刷新前、重複點擊無額外請求。驗收：`ui::scanner_refresh` 測試通過，截圖附上
  - 證據：`cargo test -p tong-funding ui::scanner_refresh`（5 passed；FakeTransport：每個來源各 +1 個行情請求、`observed_at` 晚於點擊、重複點擊 0 個額外請求、部分失敗、全部失敗）。紅燈：5 個 `not yet implemented: RefreshGate::try_begin` / `refresh_sources`。繪製：`app/src/ui/pages.rs`（虛擬化 `DataTable`）＋ `shell.rs`。**截圖待 4.1**。

## 3. 橫幅與狀態

- [x] 3.1 警示產生純函式：五類別與嚴重度排序、同來源去重、需人工處理的內容（標的、兩腿、狀態）、來源層級的過期與斷線判定、未知視為異常、停機原因。先紅後綠：窮舉三個人工處理狀態各一則、相同輸入相同輸出。驗收：`ui::alerts` 測試通過
  - 證據：`cargo test -p tong-funding ui::alerts`（15 passed；`app/src/ui/vm/alerts.rs` + `alerts_tests.rs`）。紅燈：15 個 `not yet implemented: alerts::…`。
- [x] 3.2 橫幅元件與資料新鮮度指示、載入中 / 錯誤 / 空狀態：橫幅在 8 個頁面常駐、不可關閉的警示、資料庫不可用時仍顯示、指示與橫幅一致。驗收：`ui::banner` 測試通過；切換 8 個頁面的截圖證明橫幅常駐
  - 證據：`cargo test -p tong-funding ui::banner`（9 passed；8 頁版面、不可關閉、資料庫無法開啟仍顯示且檔案未改、指示與橫幅一致、載入中 / 失敗 / 空）。紅燈：9 個 `not yet implemented: banner::…`。**切換 8 頁的截圖待 4.1**。

## 4. 驗證

（4.1、4.2 需要使用者的 Mac 與 demo 金鑰；步驟見根目錄 `TODO.md` 的「ui-readonly-pages」一節。）

- [ ] 4.1 對真實 demo 帳戶逐頁截圖（總覽、持倉、系統日誌、掃幣、橫幅），與 Figma 並排對照，並更新 `design.md` 的差異對照表與「未驗證」清單（資產列對應、initial margin 欄位）。前置條件：費率與門檻需已設定（design.md Open Questions #1 決定後）。驗收：每頁一張並排圖與差異說明
- [ ] 4.2 528 列掃幣表在實際更新頻率（1 秒 WebSocket + 10 秒輪詢）下量測 p50 與 p95 幀時間，填入 `bootstrap-gpui-shell` 的量測紀錄表；未達預算則依該 change 的緩解順序處理並回報
