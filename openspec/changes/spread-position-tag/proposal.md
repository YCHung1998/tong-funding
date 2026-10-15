## Why

同一個幣在兩個交易所的價格理論上會互相靠攏。持倉期間，如果「被設為多單那一邊的價格比被設為空單那一邊低」，多單那一邊有往上補、空單那一邊有往下補的空間，價差收斂對配對有利（正期望）；反過來（多單那邊的價格比空單那邊高）價差收斂會對配對不利。使用者希望在持倉期間即時看到這個方向，用 `positive`／`negative` 標籤標示，作為平倉與檢討的輔助訊息。

這也和 8 筆實單的觀察相關：價差損益大多為負（例：WAL −4.49、ARC −3.89），其中進場時兩腿價差方向對配對不利的筆數，目前沒有任何地方標示。

## What Changes

- 新增一個純函式：給定 long 腿所在交易所的最新價格與 short 腿所在交易所的最新價格，回傳標籤與價差百分比：
  - long 價格 < short 價格 → `positive`；
  - long 價格 > short 價格 → `negative`；
  - 相等 → `neutral`；任一腿價格缺失或過舊 → 不顯示標籤（`unknown`）。
  - 價差百分比 = (short 價格 − long 價格) ÷ long 價格 × 100（正值對應 `positive`）。
- 持倉期間（配對為 `RECONCILED`，以及仍持有部位的狀態）在「持倉」頁的配對卡與「交易單」頁的進行中列表，**持續更新**顯示標籤與價差百分比（隨掃幣資料與 WebSocket 覆蓋每秒更新）。
- `positive` 以正向色調、`negative` 以負向色調顯示，`neutral`／無資料以靜音色調顯示。
- 純顯示用：不影響達標判定、進場、平倉、提早平倉的任何決策，不寫入資料庫，不新增設定。
- 不改：既有的未實現損益、IMBALANCE 標示、資金費時間線。

## Capabilities

### New Capabilities

- `spread-position-tag`：持倉期間兩腿現價價差的 positive／negative／neutral 標籤，判定規則、資料新鮮度要求與顯示位置。

### Modified Capabilities

（無）

## Impact

- 程式：新增 `app/src/ui/vm/spread_tag.rs`（純函式與測試）；`app/src/ui/vm/positions.rs` 的 `PairCard` 增加標籤欄位；`app/src/ui/vm/staged_orders.rs` 的 `RunningRow` 增加標籤欄位；`app/src/ui/trading_pages.rs`／持倉頁畫面顯示。
- 資料來源：`UiSnapshot.market` 的各所最新觀測價格（REST 輪詢，Binance 與 Bybit 另有 WebSocket 每秒覆蓋）；無新的網路請求。
- 無資料庫變更、無設定、無事件。
- 相依：與 `leg-early-exit`、`require-synchronized-settlement` 兩個提案互相獨立。
