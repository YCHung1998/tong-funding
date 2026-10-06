## Context

掃幣頁的表格是 gpui-component 0.7.1 的 `TableState<ScannerTable>`（透過 gpui-kit 的 `DataTable` 繪製）。`ScannerTable` 是 `TableDelegate`：`columns_count`／`column(ix)` 決定欄位，`render_td(row, col)` 繪製儲存格，`rows` 由 `Shell::sync_table` 在每次重算時整批替換（已有頻率上限）。該函式庫本身已有：
- 欄位 `sortable()`／`ascending()`／`descending()`，點標題時依「預設 → 降冪 → 升冪 → 預設」循環，並呼叫 `delegate.perform_sort(col_ix, sort, …)`，排序圖示由 `column.sort` 決定；
- `fixed_left()` 固定左側欄，標題與資料列共用 `horizontal_scroll_handle`，並有橫向捲軸；
- 但沒有欄位顯示/隱藏（由 delegate 自己決定欄位清單）。

現有的 12 個欄位定義是 `SCAN_COLS`（key、標題、寬度），`render_td` 以邏輯欄位編號 0..11 對應；Candidate 儲存格（`candidates`）與 `rows` 同序，由 `Shell::candidate_cells(&rows)` 產生。

限制：表格重算與其頻率上限、預設排序規則、達標判定都屬於 `scanner-page`／`core`，本 change 不改。

## Goals / Non-Goals

**Goals:**
- 欄位顯示/隱藏、橫向捲動與固定左側欄、點標題排序，且三者可同時使用。
- 所有規則（可見性、排序鍵、缺值處理、穩定排序）是純函式，可單元測試，不依賴 GPUI。
- 狀態在刷新與切頁後保留；點「加入」永遠對應到畫面上那一列。

**Non-Goals:**
- 不做欄位拖曳排序、欄寬調整（保持現有固定寬度）。
- 不做多欄位排序。
- 不寫入資料庫或設定檔（v1 只在執行期間保留）。
- 不改預設排序、Rank 的定義、達標判定、Candidate List。

## Decisions

**D1 純函式模組 `ui/vm/scan_view.rs`。** 包含：
- `ScanColumn`（12 個欄位的列舉，含 key、標題、寬度、是否可排序、是否固定、是否可隱藏），取代 `SCAN_COLS` 的常數陣列，順序即顯示順序；
- `ColumnVisibility`（已隱藏集合；`toggle(col)`、`reset()`、`visible() -> Vec<ScanColumn>`，Symbol 的 `toggle` 為 no-op）；
- `SortState { column: ScanColumn, dir: Asc | Desc } | None` 與 `sort_rows(rows, &SortState) -> Vec<ScanRow>`。
理由：行為都在純函式內，測試不需要視窗；GPUI 層只負責接線。

**D2 排序在 `Shell::sync_table` 內、`candidate_cells` 之前執行。** `sync_table` 取得 `scanner.visible(only_qualified)` 的列後，若有 `SortState` 就排序，再算 Candidate 儲存格並寫入 delegate。這樣 `candidates` 與 `rows` 永遠同序，點「加入」對應正確的標的；排序也自然受「只顯示達標」篩選與既有重算頻率上限約束。倒數每秒更新只更新 `now_ms`，不呼叫 `sync_table`，所以不排序。

**D3 排序鍵與缺值。** 鍵為 `Option<…>`，`None` 一律排最後（升冪降冪皆然，所以比較器先比有無、再比值，降冪只反轉「有值之間」的比較）。相同鍵以預設名次 `rank` 由小到大為最後比較，等價於穩定排序，也不會因為輸入順序改變而跳動。Rank 欄顯示的仍是 `ScanRow.rank`（預設名次），不重新編號。結算倒數的鍵是 `countdown_target` 的時間戳，不是剩餘秒數。

**D4 欄位對應。** delegate 持有 `visible: Vec<ScanColumn>`；`columns_count` 回傳其長度，`column(ix)` 由 `visible[ix]` 建立（寬度、標題、`sortable()`、`fixed_left()`，並依 `SortState` 設定 `ascending()`／`descending()` 讓圖示在 `refresh` 後仍正確），`render_td` 先把 `col` 轉成 `visible[col]` 再沿用現有的繪製分支。`perform_sort(col_ix, sort)` 同樣先轉換，再寫入 `SortState`（`Default` → `None`）並請 Shell 重算。因為 delegate 不能直接碰 Shell，沿用現有 `toggles` 的做法：共享一個 `Rc<RefCell<…>>` 命令佇列，由 Shell 在下一個 tick 取出並呼叫 `sync_table`。

**D5 固定欄。** Rank 與 Symbol 設 `fixed_left()`。函式庫要求固定欄在最左側連續排列，而現有欄位順序 Rank、Symbol 本來就在最前面，所以不需改欄位順序；Rank 被隱藏時只剩 Symbol 為固定欄。這是在函式庫實際行為上的假設，列為 task 的前置驗證（見 Risks）。

**D6 欄位按鈕列。** 放在現有 `controls`（只顯示達標、來源狀態、立即刷新）與表格之間，一排可換行的小按鈕。已隱藏的按鈕用 `TEXT_MUTED` 加刪除線以外的樣式（不依賴字型的刪除線）：顯示中為 `ACCENT` 邊框與一般文字色，已隱藏為無邊框與 `TEXT_MUTED`。顏色一律走 `design-tokens`，不使用色碼字面值（`no_color_literals` 測試會檢查）。「重設欄位」按鈕在全部欄位都顯示時呈現為不可用樣式。

**D7 狀態存放。** `ColumnVisibility` 與 `SortState` 放在 `Shell` 的欄位中（不是 delegate），切頁時 Shell 不會被銷毀，所以自然保留；不寫入 store（v1）。

**D8 排序成本。** 528 列、每列以 `Option<Decimal>`／字串為鍵，單次 `sort_by` 為 O(n log n)；比較器不分配記憶體（字串比較用不分大小寫的逐字元比較，不先 `to_lowercase`）。以測試中的計時斷言（5 ms）與既有的 `--bench-table` 守護；效能量測依先前決定，實機量測記在 `TODO.md`。

## Risks / Trade-offs

- **固定左側欄與橫向捲動的實際渲染**可能與預期不符（例如固定欄背景透出下方內容、與 `stripe` 或標題對齊錯位）。緩解：實作順序先做一個最小驗證（task 4.1），用 `tools/window_shot.sh` 擷取視窗截圖確認；若不可用，降級為不固定、只做橫向捲動，並更新 spec 的「固定左側欄」為非必要。
- **函式庫的排序循環是「預設→降冪→升冪→預設」**，與本 spec 的循環（降冪→升冪→預設，從預設出發）一致，不需自訂；但 `perform_sort` 的 `col_ix` 是「可見欄位」編號，必須先轉換為邏輯欄位，否則隱藏欄位後會排錯欄。緩解：轉換函式為純函式並有測試。
- **欄位重建時排序圖示**：`refresh` 會重新呼叫 `column(ix)`，若沒有把目前的 `SortState` 寫回 `Column.sort`，圖示會消失。緩解：D4 規定 `column()` 依 `SortState` 設定；測試純函式 `column_sort_of(state, col)`。
- **字串排序**：Symbol 全是 ASCII（`BTCUSDT`、`1000PEPEUSDT`），不分大小寫比較足夠；不引入 Unicode 排序相依。

## Implementation Notes

- **版面問題（實機截圖發現，task 4.1）**：`content`（頁面容器）是 flex 子項目但沒有 `min_w_0`，內容比視窗寬時它不會縮小，整頁被撐寬後被視窗邊緣裁掉，表格自己的橫向捲動永遠不會啟動，右側欄位也點不到。修正：`content` 加 `min_w_0`、表格容器加 `w_full().min_w_0()`。測試：`the_table_stays_inside_the_window_and_the_overflow_scrolls_inside_it`（修正前表格容器延伸到 1688 px，視窗 1000 px）。
- **標題文字可點**：gpui-component 只在點小圖示時排序；標題文字點擊由 `render_th` 另外處理（`CycleSort`），圖示點擊走 `perform_sort`（`SetSort`），兩者共用同一個 `ScanViewState`。
- **固定左側欄的證據**：`scrolling_brings_the_last_column_into_view` 在每次捲動前斷言第 0、1 欄（Rank、Symbol）仍被繪製，中間欄被捲出。
- **無視窗的 Shell 測試**：以 gpui-kit 的 `test-support`（只在 dev-dependency 啟用）依元素 id 模擬點擊與捲動；元素以 `.test_support()` 註冊（正式版為 no-op）。
- **測試輔助函式的教訓**：只讀 delegate 狀態會看不出「Shell 狀態被重設但尚未同步到表格」；`Rig::view` 現在同時比對 `Shell.view` 與 delegate。

## Open Questions

- 欄位與排序要不要在重啟後保留？v1 預設「不保留」，若之後需要，再用 store 的 config 鍵 `ui.scan_table`（需通過寫入端的秘密檢查，內容只是欄位名稱）另開 change。
