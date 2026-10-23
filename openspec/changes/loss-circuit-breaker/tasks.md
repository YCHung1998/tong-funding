## 1. 核心（先寫失敗測試）

- [x] 1.1 熔斷計算純函式（累計、連虧、獲利重置、未知即觸發、門檻邊界）與測試
- [x] 1.2 `risk.rs` 新增兩欄（預設 15／3、驗證、舊設定以預設載入、不影響完整性）；Live 新資料庫預設 5／2；測試

## 2. 引擎

- [x] 2.1 區間起點：最近一筆 `KILL_SWITCH_CHANGED {on:false}`，否則首次啟動寫 `LOSS_BREAKER_BASELINE`；測試（升級不追溯、重新開放即重置）
- [x] 2.2 逐筆金額：PnL 淨額，否則由 `assemble` + `compute_pnl` 重算「價格損益 − 手續費」，否則未知；測試
- [x] 2.3 評估時機（`CLOSE_CONFIRMED` 後、PnL 寫入或重算後、啟動時）與觸發效果（kill switch + `LOSS_BREAKER_TRIPPED` 同一交易、Live 解除武裝、通知、同區間只觸發一次）；流程測試
- [x] 2.4 發布狀態列所需的引擎狀態（交易許可與停止原因、觸發模式、排隊與最近進場時間、熔斷狀態、需處理件數）

## 3. 頁面

- [x] 3.1 狀態列：中文環境標示、交易許可與原因、觸發模式、排隊與倒數、熔斷額度、需處理件數；view-model 測試
- [x] 3.2 風控頁：熔斷兩欄的輸入、驗證訊息與說明
- [x] 3.3 風控頁分組與固定儲存列（未儲存提示、停用原因）；測試

## 4. 驗證

- [x] 4.1 `cargo test` 全套綠燈、`cargo clippy` 無新增警告、`openspec validate loss-circuit-breaker --strict` 通過

## 5. 審查第一輪修正

- [x] 5.1 A2 人工確認而 FINALIZED 的配對計入（取 `CLOSE_CONFIRMED` 與轉入 FINALIZED 中較早者；watermark 也監看；模擬不計）
- [x] 5.2 A3 既有 Live 資料庫缺熔斷欄位時，啟動一次性補寫 5／2（寫 `RISK_CONFIG_UPDATED`），已有值不動，Demo 不變
- [x] 5.3 A4 Live 武裝期間熔斷兩欄納入 `FrozenCaps`（只准調低）；watermark 加入 `RISK_CONFIG_UPDATED`，改門檻後立即重評
- [x] 5.4 A5 kill switch 已開時達到門檻仍寫 `LOSS_BREAKER_TRIPPED` 並通知，狀態列顯示「熔斷」
- [x] 5.5 A6 區間起點只採用「前一個狀態為 ON」的 `KILL_SWITCH_CHANGED {on:false}`
- [x] 5.6 A9 熔斷說明與通知文案：提前出場會暫停、排程出場照常；不承諾系統通知
- [x] 5.7 A12 熔斷讀取失敗（含 watermark）失敗即封閉；`UNREADABLE` 通知、寫鎖定原因；三處鎖定共用 `lock_new_entries`
- [x] 5.8 A13 `KILL_SWITCH_CHANGED` 帶 `cause`，狀態列改讀 cause
- [x] 5.9 簡化：刪除觸發後多餘的第二次 assess、`force` 參數與重複的 `breaker_check`（評估放在 `dispatch(Tick)` 之前）；刪除 `EngineStatus.trading_allowed`

## 6. 審查第二輪修正

- [x] 6.1 B3 Live 武裝中熔斷取 min(儲存值, 凍結值)；Live 讀不到欄位時用 5／2；`config set` 寫入前取得實例鎖，App 執行中拒絕（`config show` 不取鎖）；測試：武裝中資料庫被改高仍以凍結值觸發、鎖被佔用時 CLI 拒絕
- [x] 6.2 B5 A3 回填：Live 資料庫完全沒有 risk 列時也寫入 5／2；回填失敗寫 `LIVE_BREAKER_BACKFILL_FAILED`、維持鎖定並通知，不再 `let _ =` 吞掉
- [x] 6.3 B6 `caps_hash`（改 `caps-v2`）與 `LIVE_ARMED` payload 納入兩個熔斷值（UI 與引擎同一函式）
- [x] 6.4 B7 watermark 改監看 `MANUAL_CONFIRM_RESULT`，不再對全部 `PAIR_TRANSITION` 做 `json_extract`；計入規則不變
- [x] 6.5 簡化：`lock_live` 改呼叫 `lock_new_entries`
