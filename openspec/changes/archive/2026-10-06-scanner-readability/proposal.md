## Why

1. 掃幣表格雖然開了 `stripe(true)`，但深淺交錯實際上看不出來：`component_theme` 用 `Theme::global_mut` 改 `colors`，沒有同步到 gpui-component 實際繪製用的 `tokens`，畫出來的是函式庫預設的近黑 + 40% 透明近黑；就算同步了，`BG_DEEPEST` 與 `BG_BASE` 也幾乎一樣深。長時間看表容易對錯列。
2. 頁首只顯示「Net Edge 門檻 %：0.0500」，使用者看不出一個標的要多大的資金費率價差才會達標，也不知道是哪個風控欄位造成的，因此不知道該去風控設定調哪一個。

## What Changes

- 修正 theme 同步（改用 `Theme::update`），並新增表格專用的一組列底色（奇數列 `#0B1016`、偶數列 `#182430`、hover 另一色），讓奇偶列清楚交錯。
- 掃幣頁頂端新增「達標計算」區塊：每個啟用的交易所組合一行，用實際數字列出
  `所需費率價差 % = 門檻 a + 手續費 2×(費率L + 費率S) b + 滑價 4×滑價 c + 安全邊際 d = ?`
  並註明每項對應的風控欄位名稱，旁邊連到風控設定。
- 若 `min_expected_net_pnl_pct` 這條件比門檻更嚴，該行同時列出第二條式子並標示「以較嚴者為準」。
- 必填設定缺失時該行顯示缺少哪些欄位，而非數字。

## Capabilities

### New Capabilities
- `scanner-threshold-breakdown`: 掃幣頁的達標所需價差公式展開與表格斑馬紋。

### Modified Capabilities
- `design-tokens`: 新增表格斑馬紋色 token（不改既有色值）。

## Impact

- `app/src/ui/theme.rs`：新增 `TABLE_ROW` / `TABLE_STRIPE` / `TABLE_HOVER` token；`app/src/ui/component_theme.rs` 改 `Theme::update`。
- `app/src/ui/vm/scanner.rs`：新增純函式 `threshold_breakdown(settings, enabled) -> Vec<BreakdownLine>`，重用既有 `effective_for_pair`，附測試；core 的 Net Edge 公式不變。
- `app/src/ui/shell.rs`、`pages.rs`：渲染區塊。
