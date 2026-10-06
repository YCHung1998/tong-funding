## 1. 欄位定義與可見性（純函式）

- [x] 1.1 新增 `app/src/ui/vm/scan_view.rs`：`ScanColumn` 列舉（12 欄；key、標題、寬度、可排序、固定左側、可隱藏），取代 `pages.rs` 的 `SCAN_COLS`；`pages.rs` 改由它取得欄位資料，行為不變（既有掃幣頁測試仍綠）
- [x] 1.2 `ColumnVisibility`：`toggle`、`reset`、`visible()`；Symbol 的 `toggle` 為 no-op；測試：隱藏/恢復、重設、Symbol 不可隱藏、可見欄位保持原順序、`is_all_visible`
- [x] 1.3 可見欄位編號 ↔ 邏輯欄位的轉換純函式與測試（隱藏中間欄位後轉換正確）

## 2. 排序（純函式）

- [x] 2.1 `SortState` 與各欄位的排序鍵（Rank／Symbol 不分大小寫／覆蓋／倒數目標時間／三所 rate／Gross／Net Edge／達標）；`None` 一律最後；相同鍵依預設名次；`sort_rows` 不改 `rank` 欄值
- [x] 2.2 測試（先紅後綠）：升降冪、缺值升降冪都在最後、穩定性（相同鍵）、Rank 不重編、達標欄兩組、倒數鍵不隨 `now_ms` 變、`DATA_ERROR` 的 rate 視為缺值、空列與單列
- [x] 2.3 效能測試：528 列對每個可排序欄位排序，單次不超過 5 ms（取多次中位數，避免偶發抖動）

## 3. 接進表格與 Shell

- [x] 3.1 `ScannerTable`（`pages.rs`）：持有可見欄位與目前 `SortState`；`columns_count`／`column(ix)`／`render_td` 以可見欄位轉換；`column()` 依狀態設定排序圖示；`perform_sort` 轉換欄位並寫入命令佇列
- [x] 3.2 `Shell`（`shell.rs`）：保存 `ColumnVisibility`、`SortState`；取出命令佇列後呼叫 `sync_table`；`sync_table` 先排序再 `candidate_cells`（確認 Candidate 與列同序）
- [x] 3.3 測試（`scan_view_tests.rs` 的 `pipeline` 模組，以純函式 `table_rows` 驅動真實的 `scanner::build`）：只顯示達標＋排序；`NoneQualified` 時狀態不丟；刷新後同一狀態排新資料；隱藏欄位不影響達標與順序。排序後 Candidate 與列對齊由結構保證（`sync_table` 先 `table_rows` 再 `candidate_cells`，點擊以 symbol 為鍵）；切頁保留由設計保證（狀態在 `Shell`，`go()` 只改 `page`），並在 5.2 以實機操作確認（Shell 沒有無視窗的測試框架）

## 4. 橫向捲動與固定欄（含前置驗證）

- [x] 4.1 前置驗證（結果：固定左側欄可用，見下）：Rank、Symbol 設 `fixed_left()`，視窗縮窄時以 `tools/window_shot.sh` 擷取視窗截圖，確認橫向捲軸出現、固定欄不透出下方內容、標題與資料列對齊；不通過則降級為只做橫向捲動並修改 spec 與 design 的對應敘述
- [x] 4.2 隱藏欄位使總寬小於可視寬度時不出現橫向捲動（純函式 `total_width_shrinks_when_columns_are_hidden_and_decides_horizontal_scroll`；UI 層的版面限制由 `the_table_stays_inside_the_window_and_the_overflow_scrolls_inside_it` 驗證）

## 5. 欄位按鈕列

- [x] 5.1 在 `controls` 與表格之間加入欄位按鈕列（每欄一顆，可換行）與「重設欄位」；顯示中／已隱藏樣式走 design-tokens，通過 `app/tests/no_color_literals.rs`
- [x] 5.2 實機截圖（視窗限定擷取，`HOME` 指向暫存目錄不碰真實資料庫）：預設狀態與版面修正前後各一張已取得；隱藏/排序/捲動後的截圖因為在使用中的桌面上以真實滑鼠點擊、滾輪不穩定（視窗常失焦、大小被改變）而沒有取得，改以無視窗的 `scan_table_ui_tests.rs`（真實 Shell、真實 hit-test 點擊）驗證，請 Eason 以 `cargo run -p tong-funding` 自行目視確認

## 6. 收尾

- [x] 6.1 `openspec validate scan-table-column-controls`、`cargo test --workspace`（私有 target）全綠；在 `TODO.md` 記錄「排序後 `--bench-table` 幀時間」的實機量測待辦；更新 `TASKS.md`
