## Context

- `shell.rs` 以 `DataTable::new(&self.table).stripe(true)` 繪製掃幣表；gpui-component 的斑馬列用 `cx.theme().tokens.table_even`（`table/state.rs:2032`），表底用 `tokens.table`。
- `component_theme::apply` 寫的是 `Theme::global_mut(cx).colors.*`；gpui-component 文件註明 `global_mut` 不會同步 `tokens`，只有 `Theme::update` 會。因此目前實際繪製的是函式庫預設色。
- 門檻：`RiskConfig.net_edge_threshold_pct` 是使用者直接設定的單一值；每組交易所的有效值由 `effective_for_pair`（取兩腿覆寫與全域的保守值）決定，掃幣 VM 的 `params_for` 已在用。UI 的「達標」另要求 `meets_min_expected_net_pnl`（Net Edge 扣安全邊際前 ≥ `min_expected_net_pnl_pct`）。

## Goals / Non-Goals

**Goals:** 斑馬紋看得出來；使用者一眼看懂「要多大的費率價差才達標、是哪幾個設定加出來的」。

**Non-Goals:** 改 Net Edge 公式或達標條件；逐列展開每個標的的 Net Edge（之後可另開 change）；成交量、白名單等非數值條件的展開（只在區塊下方以一行文字列出）。

## Decisions

1. **公式方向：「所需費率價差」**（使用者選定）。由 `net_edge_pct = spread − 2(fL+fS) − 4s − m ≥ T` 移項得 `spread ≥ T + 2(fL+fS) + 4s + m`。與 core 公式等價，不新增計算邏輯，只是展開顯示。
2. **純函式 `threshold_breakdown(settings, enabled_exchanges) -> Vec<BreakdownLine>`** 放在 `scanner.rs`：每個無序交易所組合一行（手續費加總與覆寫取保守值皆對稱，方向無關）；`BreakdownLine` 含各項 `(欄位名, Decimal)`、結果、可選的第二條式子、或缺少欄位清單。用 `Decimal` 計算，顯示時 `format::fixed(v, 4)`。
3. **斑馬紋色（使用者要求另選一組可分開的顏色）**：新增表格專用 token，奇數列 `TABLE_ROW = 0x0B1016`、偶數列 `TABLE_STRIPE = 0x182430`。亮度比 1.21（原 1.02）；弱化文字 `#7A8CA2` 在 `#182430` 上對比 4.57:1，仍符合 4.5:1。hover 另定 `TABLE_HOVER`（實作時選一個與兩者皆可分、且三階文字對比 ≥ 4.5:1 的值，以測試鎖定）。對比度以既有 `theme_tests` 的計算函式驗證。
4. **`component_theme` 改 `Theme::update`**：同一次把所有顏色同步進 tokens；加一個測試讀 `tokens.table_even` 斷言等於 `TABLE_STRIPE`（若 gpui-component 的 Theme 不能在無視窗測試中建立，改為實機截圖取色驗證，並在 tasks 記錄）。

## Risks / Trade-offs

- [`Theme::update` 同步後，其他原本「意外」使用預設色的元件顏色也會改變] → 實機逐頁截圖檢查（掃幣、持倉、交易單、系統日誌、所有輸入框）。
- [三個交易所 → 最多 3 行公式，佔用頁首空間] → 區塊可收合，預設展開；只列啟用的交易所組合。
