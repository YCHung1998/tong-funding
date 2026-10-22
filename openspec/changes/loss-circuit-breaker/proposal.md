## Why

目前沒有任何「虧到多少就停」的機制：kill switch 只擋新單、要人手動打開；17 筆 demo 實單 3 勝 14 負、合計 −49.40 USDT，系統照樣繼續進場。
使用者已決定熔斷門檻（2026-10-22）：**Demo 累計虧損 15 USDT 或連虧 3 筆、Live 階段 2 為 5 USDT 或連虧 2 筆**，觸發後自動停止開新倉、平倉照常。
同時，畫面評估發現關鍵運作狀態不顯眼或語意不清（AUTO 只在交易單頁、狀態列寫「Kill switch 未啟用」、沒有下一次結算與排隊數），以及風控頁所有欄位擠在一起、儲存按鈕在畫面外。

## What Changes

- **熔斷**：自「最近一次開放交易」（kill switch 關閉，Live 武裝亦同）起算，逐筆累計已平倉配對的損益；累計虧損達上限或連虧達筆數 → 自動打開 kill switch（只擋開新倉），寫 `LOSS_BREAKER_TRIPPED` 並通知，Live 同時解除武裝。
  - 每筆金額：PnL 淨額已知用淨額；未知時用「價格損益 − 手續費」（不計資金費，偏保守）；仍未知 → 直接觸發（失敗即封閉）。
  - 首次啟用時寫 `LOSS_BREAKER_BASELINE`，之前的歷史不計入。
- **新設定欄位**：`loss_breaker_max_loss_usdt`（預設 15）、`loss_breaker_max_consecutive_losses`（預設 3）；新的 Live 資料庫預設 5 與 2；舊設定缺欄位時以預設值載入，不會變成「不完整」。
- **狀態列（每頁固定）**：交易允許／已停止（附原因：手動、熔斷、看門狗、Live 未武裝）、觸發模式（自動／手動）、排隊配對數與最近一筆的進場倒數、熔斷額度（已虧 a／上限、連虧 b／上限）、需處理件數；模式改用中文標示，取代 `EXCHANGE_DEMO` 與「Kill switch 未啟用」。
- **風控頁版面**：欄位分組（進場條件、成本估計、執行保護、部位與熔斷）、熔斷欄位、固定在底部的儲存列並提示未儲存的變更。

## Capabilities

### New Capabilities
- `loss-circuit-breaker`: 熔斷的計算區間、計入規則、觸發與效果、狀態公開。
- `risk-page-layout`: 風控頁分組與固定儲存列。

### Modified Capabilities
- `risk-config`: 新增兩個熔斷欄位（預設值、驗證、Live 新資料庫預設）。
- `app-shell`: 狀態列新增交易許可、觸發模式、排隊與倒數、熔斷額度、需處理件數。

## Impact

- `core/src/`：熔斷計算純函式；`risk.rs` 新欄位與驗證。
- `app/src/engine/`：基準事件、逐筆金額（`pnl_record::assemble` + `compute_pnl`）、觸發與 kill switch、狀態發布；`live_guard.rs` 的 Live 預設。
- `app/src/ui/`：`shell.rs` 狀態列、風控頁（`trading_pages.rs`、`vm/risk_settings.rs`）。
- 不在本 change：風控方向徽章的紅綠配色（受 `risk-settings-guidance` 的規格約束，另案處理）、提前出場。
