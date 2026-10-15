## 1. 設定

- [x] 1.1 在 `core/src/risk.rs` 的風控設定新增選填欄位 `early_exit_min_move_pct`（Decimal，≥ 0，預設 0.3，0 = 停用；缺欄位的既有設定載入時取預設值）；先寫紅色測試（序列化、驗證負值被拒、缺欄位的既有設定取得預設 0.3、不影響「完整性」判定）再實作
- [ ] 1.2 在 `app/src/ui/vm/risk_settings.rs` 與設定頁新增該欄位與說明文字（沿用 `min_expected_net_pnl_pct` 的做法），補 vm 測試

## 2. 排程判斷（純函式）

- [x] 2.1 在 `app/src/engine/timings.rs` 新增 `early_exit_start_ms = 5_000`；在 `app/src/engine/schedule.rs` 新增純函式：給定 T、校正後時間、每腿開倉均價與目前價格、方向、門檻，回傳各腿是否達標；紅色測試涵蓋：門檻等於即通過、窗外（T+4.9 秒、T+15 秒）不動作、校時缺失不動作、價格過舊不採用、門檻 0／未設定不動作、多空兩個方向
- [x] 2.2 實作 2.1，全部轉綠

## 3. 引擎：單腿平倉

- [x] 3.1 在 `app/src/engine/actor.rs` 的 `tick_reconciled` 於窗內對尚未平倉的腿抓最新價格（`refetch`），僅在 `AUTO` 且 kill switch 未啟用時呼叫 2.1；先寫紅色測試（用既有的 flow_tests 腳本化交易所）：只平達標的一腿、另一腿不動、配對仍為 `RECONCILED`、寫入 `LEG_EARLY_EXIT` 事件
- [x] 3.2 實作單腿 `OrderSet`（重用 `on_close_positions` 的數量與不平衡檢查）；單腿被拒或逾時只寫事件、不改配對狀態；補測試
- [x] 3.3 兩腿都已平倉時直接進入既有「確認已平倉」流程且 T+15 不再送單；只有一腿提早平倉時 T+15 只平另一腿；補測試
- [x] 3.4 重啟對帳：配對為 `RECONCILED` 但一腿已平時不誤判為不平衡；先寫測試確認現況，若會誤判則在此修正

## 4. 損益與驗證

- [ ] 4.1 `app/src/funding/pnl_record.rs`：兩腿平倉時間不同時損益拆解與資金費視窗正確，補測試（`pnl_record_tests.rs`）
- [ ] 4.2 `openspec validate leg-early-exit` 通過；`cargo test -p tong-funding-core -p tong-funding` 全綠，記錄指令與通過數量
