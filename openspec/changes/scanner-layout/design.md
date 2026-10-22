## Context

- 欄位與欄位按鈕由 `scan-table-column-controls`（已完成未封存）定義：12 欄、順序固定、「加入交易單」在最右、Rank 與 Symbol 固定在左。
- rate 儲存格的內容由 `ui-readonly-pages`（未封存）規定：rate（小數 4 位）、週期標籤、非 8h 時的 8h 等效次要文字，顏色使用 design-tokens 的 funding rate 語意。
- 主規格 `design-tokens`「Funding rate 與趨勢的顏色語意」只規定「大於 0 為正向色、小於 0 為負向色、等於 0 為弱化文字色」，沒有規定顏色；目前實作把正向色、負向色對應到綠、紅。

## Goals / Non-Goals

**Goals:** 預設寬度下主要操作可見、無截斷、費率不再用好壞色。

**Non-Goals:** 不改欄位順序、不新增欄位、不改排序與篩選；不改 Net Edge 的紅綠（那是好壞）。

## Decisions

1. **騰出寬度的方法**：停用交易所的欄預設隱藏（使用者手動顯示的選擇要被記住，不被覆蓋）；rate 欄改兩行（第一行 rate＋週期、第二行 8h 等效），以降低單行寬度；Gross 欄寬依內容。
2. **費率顏色**：新增 `RATE_POSITIVE`、`RATE_NEGATIVE` 兩個 token（例如琥珀與青色，對比度 ≥ 4.5:1），funding rate 顏色函式改用它們；`SUCCESS`／`DANGER` 不變，仍用於 Net Edge 與損益。
3. **預設寬度**：以目前截圖的視窗寬度（約 1,850 邏輯像素、字級 100%）為驗收基準，headless UI 測試鎖住「最右兩欄在可視範圍內」。

## Risks / Trade-offs

- [兩行 rate 讓列高增加、同畫面列數變少] → 可接受；換得不截斷。
- [使用者放大字級後仍可能需要橫向捲動] → 既有的固定左欄與橫向捲動保留。
