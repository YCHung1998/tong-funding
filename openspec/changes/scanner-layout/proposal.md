## Why

畫面評估（2026-10-22）中掃幣頁的問題：主要操作「達標」與「加入交易單」兩欄在預設視窗寬度下被擠到畫面外；Gross 欄的型態標籤被截斷並與 Net Edge 欄重疊；Binance、Bybit 欄的「8h 等效」數值被截斷；費率用紅綠上色暗示好壞（但負費率對多方是收入）；資料來源狀態列擠在一起、每個來源出現兩次「ONLINE」；已停用的 OKX 仍佔一欄且狀態為紅色 OFFLINE。

## What Changes

- **主要操作可見**：預設視窗寬度下，不需橫向捲動即可看到「達標」與「加入交易單」：不在 `allowed_exchanges` 的交易所欄預設隱藏（使用者仍可用欄位按鈕顯示），其餘欄寬依內容收斂。
- **不截斷**：Gross 欄完整顯示數值、「週期不同」與型態標籤；rate 儲存格完整顯示 rate、週期標籤與 8h 等效值（可換成兩行）。
- **費率改用中性顏色**：funding rate 的正、負色改為不帶好壞含意的兩種顏色（非紅綠）；紅綠只留給 Net Edge 與損益。
- **狀態列整理**：「只顯示達標」開關與資料來源之間有明確間隔；每個來源只顯示一次狀態與距今秒數；已停用的交易所顯示「已停用」弱化文字而不是紅色 OFFLINE。

## Capabilities

### New Capabilities
- `scanner-layout`: 掃幣表在預設寬度下的可見性、不截斷、狀態列。

### Modified Capabilities
- `design-tokens`: funding rate 的正、負向色改為中性的兩種顏色。

## Impact

- `app/src/ui/pages.rs`（掃幣表）、`app/src/ui/vm/scanner.rs`、`scan_view.rs`、`theme.rs`。
- 不改排序、篩選、達標判定或加入交易單的行為；欄位順序不變（`scan-table-column-controls` 的規格：加入交易單仍在最右）。
