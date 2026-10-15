## Why

目前每一個配對都固定在結算後 T+15 秒把兩腿一起平倉。結算（T）之後，價格往有利方向走的那一腿，如果等到 T+15 才平，可能把已經出現的價差獲利又吐回去。使用者希望在 T+5 到 T+15 這段窗口內，每一腿只要相對自己的開倉價已經往有利方向走出足夠幅度，就可以先平掉，保留獲利；沒有被提早平掉的腿照舊在 T+15 平倉。

資料佐證：2026-10-14／15 八筆實單的價差損益大多為負（例：WAL −4.49、ARC −3.89），其中不少是持倉的 25 秒內其中一腿逆向走；但也有單腿有利的情形（RSR 的 Bybit 腿 +0.96、BLUR 04:00 的 Bybit 腿 +0.90），這些有利幅度在 T+15 前是否保得住沒有被利用。

## What Changes

- 新增「單腿提早平倉」：配對為 `RECONCILED`、現在落在 T+5 ≤ 時間 < T+15（以兩交易所校正後的時間判斷）時，**每一腿各自判斷**：
  - 多單：目前價格相對該腿開倉成交價的漲幅 ≥ `early_exit_min_move_pct`，就只平這一腿。
  - 空單：目前價格相對該腿開倉成交價的跌幅 ≥ `early_exit_min_move_pct`，就只平這一腿。
- 被提早平掉的腿記錄為已平倉；另一腿照原規則在 T+15 平倉，或在窗內自己也達到門檻時提早平倉。兩腿都已平倉時，配對進入既有的「確認已平倉」流程。
- 新增風控設定 `early_exit_min_move_pct`（百分比數值）。**沒有填寫（或設為 0／關閉）時不啟用提早平倉**，行為與現在完全相同。
- 提早平倉只用 reduce-only 市價單，數量取該腿記錄成交量與實際持倉較小者，沿用現有平倉的數量與不平衡檢查。
- 提早平倉僅在 `trigger_mode = AUTO` 且 kill switch 未啟用時執行（單腿先平會暫時留下單邊曝險，不屬於「只降低曝險」的動作，見 design）。
- 不改：進場時間（T−10）、預設 T+15 平倉、兩腿一起平的既有流程、配對狀態列舉（不新增狀態）。

## Capabilities

### New Capabilities

（無）

### Modified Capabilities

- `pair-lifecycle`：新增要求「`RECONCILED` 的配對在 T+5～T+15 可單腿提早平倉」，說明單腿平倉後配對仍為 `RECONCILED`、不被視為單腿失敗或不平衡。
- `risk-config`：新增設定 `early_exit_min_move_pct` 的欄位、驗證與「未設定即停用」的規則。

## Impact

- 程式：`app/src/engine/schedule.rs`（新增窗內判斷）、`app/src/engine/actor.rs`（單腿平倉流程、與 T+15 的整體平倉銜接、重啟後的對帳）、`app/src/engine/timings.rs`（新增 `early_exit_start_ms = 5_000`）、`core/src/risk.rs` 與風控設定頁（`app/src/ui/vm/risk_settings.rs`）新增一個欄位、損益拆解（`app/src/funding/pnl_record.rs`）需能處理兩腿不同的平倉時間。
- 資料：設定存放於既有 `config` 表的 `risk` JSON，新增一個選填欄位，無資料庫遷移；事件新增 `LEG_EARLY_EXIT`（附腿、觸發價、門檻、開倉價）。
- 風險：窗內單腿平倉後，另一腿最長約 10 秒沒有對沖；價格資料延遲或單腿平倉失敗時的處理，見 design。
- 相依：與 `require-synchronized-settlement`、`spread-position-tag` 兩個提案互相獨立，可分別實作。
