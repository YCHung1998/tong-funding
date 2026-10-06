## Why

手動下單頁要平掉一筆持倉時，使用者必須自己輸入交易所、Symbol、反方向、數量並勾選 reduce_only；撤單則要手打 `client_order_id`。手打容易打錯方向或數量，錯一次就可能反向加倉。應該讓使用者從目前持倉 / 掛單中直接點選。

## What Changes

- 手動下單頁新增「目前持倉」清單（依目前 `execution_mode` 取模擬或 demo 帳戶的持倉），每列有「帶入平倉」按鈕。
- 點選後自動填入：交易所、Symbol、反方向（多 → SELL、空 → BUY）、數量 = 持倉絕對值、`reduce_only` = 勾選。仍須經過原本的確認視窗才送出。
- 撤單區新增「目前掛單」清單，點選即填入交易所、Symbol 與 Order ID，不必手打 id。
- 持倉 / 掛單資料不完整或讀取失敗時如實顯示，不顯示過期或猜測的列。

## Capabilities

### New Capabilities
- `manual-order-picker`: 手動下單頁從現有持倉帶入平倉單、從現有掛單帶入撤單。

### Modified Capabilities
（無。`manual-order-page` 的需求仍在進行中的 `ui-trading-pages` change，本 change 以新增需求方式補充，不改動原需求。）

## Impact

- `app/src/ui/vm/manual_order.rs`：新增純函式 `open_positions(snap, mode)`、`open_orders(snap, mode)`、`close_prefill(position)`，附測試。
- `app/src/ui/trading_pages.rs`：手動下單頁渲染兩個清單與點選處理（寫入現有 `m_*` / `x_*` 輸入框）。
- 資料來源：`UiSnapshot.leg_accounts`（已每 15 秒輪詢），不新增 API 請求。
