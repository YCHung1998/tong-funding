## Context

- kill switch 存在 `system_flags`，切換時同一交易寫 `KILL_SWITCH_CHANGED {on}`；`set_kill_switch_with_event` 可同時寫原因事件（Live 的 `LIVE_ARMED`／`LIVE_AUTO_LOCK` 已在用）。Live 武裝會把 kill switch 關掉。
- 平倉確認寫 `CLOSE_CONFIRMED`；PnL 由 `pnl_record::settle_pnl` 寫 `PAIR_PNL_COMPUTED`／`RECOMPUTED`，資金費可能晚到（Bybit 曾延遲 3 小時，issue 003），所以 FINALIZED 可能晚很久。
- `pnl_record::assemble` + `compute_pnl` 隨時可由資料庫重算一筆配對的價格損益與手續費（資金費缺漏時 `Net` 標為缺漏）。

## Goals / Non-Goals

**Goals:** 虧損達門檻時自動停止開新倉、不等資金費入帳；每次觸發都可追溯；使用者隨時看得到額度用了多少。

**Non-Goals:** 不自動平倉（看門狗另案）；不做日曆日的每日上限（以「開放交易後」為區間，與 Live 每次武裝的邏輯一致）；不改方向徽章配色。

## Decisions

1. **區間起點** = 最近一筆 `KILL_SWITCH_CHANGED {on:false}` 的時間；沒有的話 = 第一次啟動本功能時寫的 `LOSS_BREAKER_BASELINE`（不追溯歷史，避免一升級就因舊虧損立刻停機）。使用者重新開放交易即重置。
2. **計入時點用 `CLOSE_CONFIRMED`，不等 FINALIZED**：平倉確認後即以「價格損益 − 手續費」暫計，PnL 淨額可用時改用淨額。資金費通常是收入，暫不計入偏保守。
3. **未知即觸發**：一筆已平倉配對的價格或手續費都算不出來時，視為無法評估 → 觸發（原因寫明）。
4. **純函式**：`evaluate(amounts: &[Option<Decimal>], max_loss, max_consecutive) -> BreakerState { loss_sum, consecutive, tripped, reason }`；金額 ≥ 0 重置連虧；`Σ ≤ −max_loss` 或 `consecutive ≥ max_consecutive` 觸發。
5. **評估時機**：寫入 `CLOSE_CONFIRMED` 後、PnL 寫入或重算後、引擎啟動時。同一區間只觸發一次（kill switch 已開就不重寫事件）。
6. **觸發效果**：`set_kill_switch_with_event(true, "LOSS_BREAKER_TRIPPED", {loss_sum, consecutive, thresholds, pairs})`；Live 呼叫既有解除武裝；通知。進行中的配對照常出場。
7. **狀態列資料**：引擎發布 `{trading_allowed, stop_reason, trigger_mode, queued_pairs, next_entry_ms, breaker{loss_sum, max_loss, consecutive, max_consecutive, tripped}, needs_handling}`；停止原因由最近一筆 kill switch 事件的原因事件推得（手動、熔斷、看門狗、Live 啟動鎖定／未武裝）。
8. **風控頁分組**只改版面與儲存列，不改欄位的驗證與保守合併規則；方向徽章配色維持現狀。

## Risks / Trade-offs

- [暫計不含資金費，可能比實際更早觸發] → 偏保守是刻意的；通知寫明「暫計」。
- [PnL 重算後金額變好，已觸發不會自動解除] → 解除一律由使用者重新開放交易。
- [Live 預設 5 USDT 對每腿 10 USDT 而言約兩三筆最差情況] → 與使用者決定一致；可在風控頁調整。

## Migration Plan

舊設定以預設值補齊；首次啟動寫基準事件；回滾 = 還原 commit（基準與觸發事件保留在事件表，無害）。
