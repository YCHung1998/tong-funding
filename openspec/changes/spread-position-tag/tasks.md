## 1. 純函式

- [ ] 1.1 新增 `core/src/spread_tag.rs`（UI 與引擎共用）與測試（紅色先行）：long < short → positive 且百分比為正；long > short → negative；相等 → neutral 且為 0；任一價格缺失 → unknown；long 價格為 0 → unknown；百分比為 Decimal 且不四捨五入
- [ ] 1.2 實作 1.1，轉綠；在 `core/src/lib.rs` 註冊模組

## 2. 價格與新鮮度

- [ ] 2.1 新增共用的小函式：依 `(long_exchange, short_exchange, symbol)` 從 `snap.market` 取兩腿最新價格；來源 `failing()` 或觀測過舊（`stale_data_threshold_ms`）回傳 `None`；紅色測試涵蓋新鮮、失敗、過舊、缺資料
- [ ] 2.2 實作 2.1，轉綠

## 3. 顯示

- [ ] 3.1 `app/src/ui/vm/positions.rs`：`PairCard` 增加標籤與價差百分比欄位（僅仍持倉的狀態）；vm 測試涵蓋各狀態與行情更新後標籤改變
- [ ] 3.2 `app/src/ui/vm/staged_orders.rs`：`RunningRow` 增加標籤與價差百分比；vm 測試
- [ ] 3.3 畫面：`app/src/ui/trading_pages.rs` 與持倉頁顯示文字 `positive`／`negative`／`neutral` 與百分比，色調用既有 `Tone`；補一個 GPUI 煙霧測試（若既有的畫面測試框架可用）

## 4. 驗證

- [ ] 4.1 `openspec validate spread-position-tag` 通過；`cargo test -p tong-funding-core -p tong-funding` 全綠，記錄指令與通過數量
- [ ] 4.2 用實際資料核對（唯讀）：對 `funding.db` 的 8 筆實單，依進場快照／成交價推算當時的標籤，與價差損益並列於 `issue/`（只呈現，不推論因果）

- [ ] 3.4 （整合階段由主流程完成，避免與 `leg-early-exit` 同時改 `actor.rs`）引擎在進入 `RECONCILED` 與平倉單送出前各寫一筆 `SPREAD_TAG` 事件，沿用 1.1 的純函式；補引擎測試
