## Context

- kill switch 存在 `system_flags`，切換時同一交易寫 `KILL_SWITCH_CHANGED {on}`；`set_kill_switch_with_event` 可同時寫原因事件（Live 的 `LIVE_ARMED`／`LIVE_AUTO_LOCK` 已在用）。Live 武裝會把 kill switch 關掉。
- 平倉確認寫 `CLOSE_CONFIRMED`；PnL 由 `pnl_record::settle_pnl` 寫 `PAIR_PNL_COMPUTED`／`RECOMPUTED`，資金費可能晚到（Bybit 曾延遲 3 小時，issue 003），所以 FINALIZED 可能晚很久。
- `pnl_record::assemble` + `compute_pnl` 隨時可由資料庫重算一筆配對的價格損益與手續費（資金費缺漏時 `Net` 標為缺漏）。

## Goals / Non-Goals

**Goals:** 虧損達門檻時自動停止開新倉、不等資金費入帳；每次觸發都可追溯；使用者隨時看得到額度用了多少。

**Non-Goals:** 不自動平倉（看門狗另案）；不做日曆日的每日上限（以「開放交易後」為區間，與 Live 每次武裝的邏輯一致）；不改方向徽章配色。

## Decisions

1. **區間起點** = 最近一筆「前一個狀態為 ON」的 `KILL_SWITCH_CHANGED {on:false}` 的時間（kill switch 起始為 OFF；沒有實際切換的 OFF 不算）；沒有的話 = 第一次啟動本功能時寫的 `LOSS_BREAKER_BASELINE`（不追溯歷史，避免一升級就因舊虧損立刻停機）。使用者重新開放交易即重置。
2. **計入時點用 `CLOSE_CONFIRMED`，不等 FINALIZED**：平倉確認後即以「價格損益 − 手續費」暫計，PnL 淨額可用時改用淨額。人工確認而 FINALIZED 的配對（從 `PARTIAL_FAILURE`／`IMBALANCED`／`UNRESOLVED` 轉入）沒有 `CLOSE_CONFIRMED`，改以該轉移事件計入；每個配對取兩者中最早的一筆，第一次出現在區間起點前者屬於較早區間。資金費通常是收入，暫不計入偏保守。
3. **未知即觸發**：一筆已平倉配對的價格或手續費都算不出來時，視為無法評估 → 觸發（原因寫明）。
4. **純函式**：`evaluate(amounts: &[Option<Decimal>], max_loss, max_consecutive) -> BreakerState { loss_sum, consecutive, tripped, reason }`；金額 ≥ 0 重置連虧；`Σ ≤ −max_loss` 或 `consecutive ≥ max_consecutive` 觸發。
5. **評估時機**：引擎啟動時，以及每個 tick 在 `dispatch(Tick)` 之前（先觸發、後進場），只在監看的事件（`CLOSE_CONFIRMED`、人工確認的結果 `MANUAL_CONFIRM_RESULT`［審查第二輪 B7：取代對全部 `PAIR_TRANSITION` 的 `json_extract`；它在轉入 FINALIZED 的轉移落地之後、同一個 actor 步驟內寫入，所以接著的那次評估已看得到轉移；計入規則仍以該轉移為準］、PnL 寫入／重算、`KILL_SWITCH_CHANGED`、`LOSS_BREAKER_*`、`RISK_CONFIG_UPDATED`）的最高 id 變動時重算；`CLOSE_CONFIRMED` 寫入後也立即看一次。同一區間只觸發一次，但 kill switch 已開而本區間尚無紀錄時仍寫 `LOSS_BREAKER_TRIPPED` 並通知（狀態列才會顯示「熔斷」）。讀取失敗（含 watermark）一律失敗即封閉：鎖定、寫 `UNREADABLE`、通知。看門狗鎖定、熔斷觸發、讀取失敗三處共用同一個鎖定 helper。
6. **觸發效果**：`set_kill_switch_with_event(true, "LOSS_BREAKER_TRIPPED", {loss_sum, consecutive, thresholds, pairs})`；Live 呼叫既有解除武裝；通知。進行中的配對照常出場。
7. **狀態列資料**：引擎發布 `{trading_allowed, stop_reason, trigger_mode, queued_pairs, next_entry_ms, breaker{loss_sum, max_loss, consecutive, max_consecutive, tripped}, needs_handling}`；停止原因由最近一筆 `KILL_SWITCH_CHANGED` 的 `cause` 欄位（`set_kill_switch_with_event` 寫入原因事件類型）推得（手動、熔斷、看門狗、Live 啟動鎖定／未武裝）；不再依賴「下一筆事件 id」。`EngineStatus` 不另存 `trading_allowed`（`stop_reason` 為空即允許）。
8. **風控頁分組**只改版面與儲存列，不改欄位的驗證與保守合併規則；方向徽章配色維持現狀。

## Risks / Trade-offs

- [暫計不含資金費，可能比實際更早觸發] → 偏保守是刻意的；通知寫明「暫計」。
- [PnL 重算後金額變好，已觸發不會自動解除] → 解除一律由使用者重新開放交易。
- [提前出場在 kill switch 開啟時不動作] → 熔斷說明與通知寫「提前出場會暫停、排程出場照常」，不寫「進行中的配對照常出場」。
- [既有 Live 資料庫缺熔斷欄位會以較鬆的 15／3 載入] → Live 啟動時一次性補寫 5／2（審查第二輪 B5：連「完全沒有 risk 列」也補，第三輪 C5 起此情況寫入完整的 `live_db_defaults().risk`，否則之後 `apply_if_fresh` 永遠跳過、手動觸發與提前出場 0 不會被寫入；補寫失敗寫 `LIVE_BREAKER_BACKFILL_FAILED`、維持鎖定並通知，狀態列顯示「熔斷」，第三輪 C4）；補寫前或失敗時，Live 讀不到的欄位一律以 5／2 評估（B3）。
- [武裝期間資料庫被繞過 App 改高] → 評估取 min(資料庫值, 凍結值)；`caps_hash` 與 `LIVE_ARMED` 也納入兩個熔斷值（UI 與引擎共用 `live_guard::caps_hash`）；`config set` 命令列寫入前取得單一實例鎖，App 執行中拒絕（`config show` 唯讀不取鎖；審查第三輪 C3：改以 SQLite 唯讀旗標開啟 `ConfigReader`，不呼叫 `apply_if_fresh`、不執行遷移、檔案不存在時不建立）。
- [Live 預設 5 USDT 對每腿 10 USDT 而言約兩三筆最差情況] → 與使用者決定一致；可在風控頁調整。

## Migration Plan

舊設定以預設值補齊；首次啟動寫基準事件；回滾 = 還原 commit（基準與觸發事件保留在事件表，無害）。
