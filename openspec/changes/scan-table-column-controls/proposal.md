## Why

掃幣頁的 Funding Rate Matrix 有 12 欄、總寬約 1,300 px，視窗較窄時橫幅過長、看不到右側欄位，也沒有辦法重新排序（目前只有固定的預設排序）。使用者需要自己決定要看哪些欄位、能左右捲動看到其他資訊，並能點欄位標題依任一欄升冪或降冪排序，以便從不同角度找標的（例如先看哪個標的倒數最近、哪個交易所 rate 最高）。

## What Changes

- 表格上方新增一排「欄位按鈕」：每個欄位一顆，點一下切換顯示或隱藏；另有「重設欄位」一鍵恢復全部顯示。Symbol 欄固定顯示，不可隱藏。
- 表格可左右捲動：欄位總寬超過可視寬度時出現橫向捲動；Rank 與 Symbol 欄固定在左側（捲動時不消失），其他欄跟著捲。
- 可排序欄位的標題可點：點一下降冪、再點升冪、再點回預設排序（三態循環）。排序的是「列的順序」，Rank 欄仍顯示預設排序下的名次，不重新編號。
- 排序與欄位顯示狀態在行情刷新、切換「只顯示達標」、立即刷新時都保留。
- 本 change 只動掃幣頁的顯示層，不改 `core` 的達標判定、Net Edge、預設排序規則與 Candidate List 行為。

## Capabilities

### New Capabilities
- `scanner-table-controls`: 掃幣表的欄位顯示/隱藏、橫向捲動與固定欄、依欄位升/降冪排序。

### Modified Capabilities
<!-- 無。`scanner-page` 的欄位與預設排序需求維持不變；本 change 在其上新增互動。 -->

## Impact

- `app/src/ui/vm/`：新增純函式模組（欄位定義、可見性規則、排序鍵與比較器），含單元測試。
- `app/src/ui/pages.rs`：`ScannerTable` 的欄位對應（可見欄位 → 邏輯欄位）、`perform_sort`、標題排序狀態。
- `app/src/ui/shell.rs`：保存欄位/排序狀態、`sync_table` 先排序再算 Candidate 儲存格、欄位按鈕列。
- 不新增相依套件；使用 gpui-component 0.7.1 既有的表格排序與橫向捲動能力。
- 依賴 `feat/ui-readonly-pages` 與 `feat/ui-trading-pages` 已有的掃幣表（本 change 建立在 `19b3f05` 之上）。
