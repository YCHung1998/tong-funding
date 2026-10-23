## Context

- 進場逾時時（`FILL_MONITOR` 逾時），引擎已對未成交的委託發出撤單（`start_timeout_cancels`），並依兩腿成交狀況轉 `PARTIAL_FAILURE`／`CANCELLED`／`UNRESOLVED`（`fill.rs`）。平倉失敗則轉 `PARTIAL_FAILURE`（`CloseFailed`）。重啟時復原也可能轉 `PARTIAL_FAILURE`／`UNRESOLVED`。
- Live 已有手動的單腿平倉 `Command::FlattenLeg`（`actor/live.rs`）：讀持倉（`signed_position`）與記錄成交（`recorded_fill`，以 `client_order_id` 逐單查證，找不到或查詢失敗即為未知、不猜），以 `plan_leg_close` 決定數量（取兩者較小者；方向錯或差距超過 `max_leg_imbalance_pct` 即拒絕），先落 intent 再送 reduce-only 單，**不改變配對狀態**。
- Live 在 `PARTIAL_FAILURE` 等狀態已會自動鎖定；Demo 沒有。
- `pair-lifecycle` 規定這三個狀態沒有系統自動轉出；看門狗只送平倉單，不改狀態，因此不需改規格。

## Goals / Non-Goals

**Goals:** 確認只有一腿裸露時，在約「訂單逾時 + 5 秒」內自動平掉；查不清楚時寧可不動並停止開新倉；每一步留事件；頁面寫清楚系統會做與不會做的事。

**Non-Goals:** 不補另一腿；不處理兩腿都有部位但數量不平衡的情況（`IMBALANCED` 中兩腿皆有部位者只提示）；不改提前出場（另議）；不讓設定可調整緩衝與重試次數。

## Decisions

1. **決策是純函式**（`engine/watchdog.rs`，或 core）：輸入每腿的 `position: Result<Decimal>`、`recorded: Result<Decimal>`、`orders_terminal: Result<bool>`、容差；輸出 `Flatten { leg, qty } | Hedged | Flat | WaitForCancel | Unknown { reason }`。規則：
   - 任一腿任一輸入為 `Err`（讀取類錯誤：查詢失敗、找不到單、持倉讀不到、整輪逾時、探測任務遺失）→ `ReadFailed`（該輪只記錄、5 秒後再查，不送單、不鎖定）；第 3 輪 `ReadFailed` → `Unknown`。實質矛盾（方向錯、超出容差、持倉與記錄不符）不在此列，立即 `Unknown`。
   - 任一張單未終結 → `WaitForCancel`（引擎撤單後下一輪再查；累計 3 輪仍未終結 → `Unknown`）。
   - 計數（審查第二輪 B2）：等待撤單與讀取失敗各自累計、互不歸零（原本互相歸零，W、R 交替會無限輪詢）；送出平倉單的那一輪與結束輪詢的結論才把兩者歸零。更新只在 `finish_round` 一處（純函式 `RoundCounts::after`），重啟由事件以同一規則重建。（審查第三輪 C2）不另設每次警示的總輪數上限（原 `MAX_ROUNDS_PER_ALERT = 12` 已刪除）：輪數已由「等待撤單、讀取失敗各 3 輪即 Unknown、只有送出平倉單的那一輪歸零、最多送出 3 次」保證有界；總輪數上限反而會讓第 3 次平倉被提前判為 Unknown，與「最多送出 3 次」不符。
   - 每腿以 `plan_leg_close` 的規則判斷部位（方向錯或差距超出容差 → `Unknown`）。
   - 恰好一腿有部位、另一腿「為空」→ `Flatten`；「為空」明確指記錄成交 = 0 且持倉 = 0（不得只依 `plan_leg_close` 算出 0，容差 100 時它會把「記錄 50、持倉 0」也算成 0）；兩腿皆有 → `Hedged`（文案：兩腿都有部位（可能不平衡），列出兩腿數量）；兩腿皆無 → `Flat`。
   - 替代：沿用 `FlattenLeg` 手動路徑直接送單、不先判斷另一腿 → 無法滿足「確認另一腿確實沒成交」（使用者要求的緩衝）。
2. **時機**：配對進入警示狀態的時間 `alert_since_ms`（記憶體中由落地的轉移取得；重啟時由最後一筆轉入該狀態的 `PAIR_TRANSITION` 事件時間還原）。`now ≥ alert_since_ms + 5_000` 才第一次查證；每輪間隔 5 秒；同一配對同時只有一個查證或平倉在進行。
3. **互斥**：手動 `FlattenLeg` 讀取中（`flatten_inflight`）或手動送出的平倉單未終結且在訂單逾時內時，看門狗不開新輪；看門狗查證／平倉中，手動被拒，手動的讀取回來時若看門狗已接手則放棄；`send_flatten` 發現同一腿已有「未終結且未逾時」的平倉單就拒絕，不覆寫 `flow.close.legs[leg]`。審查第二輪 B1：原本把 `flow.close` 裡任何非看門狗的未終結單都當成使用者平倉，一般平倉流程結果未知（查詢一直 NotFound）的殘單讓看門狗永遠不跑、手動也被永久拒絕。現在 flow 以 `flattens`（`client_order_id`、`by`、`deadline_ms = fill::timeout_at_ms`）記錄每張送出的平倉單，`user_flatten_active` 只看 `by = Manual` 且未逾時者；`send_flatten` 的拒絕對無記錄的單以其所屬單組的訂單逾時為期限。逾時或結果未知的單交給看門狗那一輪查證（NotFound → ReadFailed → … → Unknown 鎖定）；殘單可能晚到成交的風險由「新單數量取自當下重讀的持倉、reduce-only」承擔。
3b. **平倉路徑共用**：把 `flatten_leg`／`on_flatten_position` 的送單部分抽成「系統與 Live 手動共用」；手動按鈕維持 Live 限定。送出前先落 intent（落不了就不送）；`LEG_FLATTEN` 加 `by: "watchdog" | "manual"`。
4. **重試**：送出的平倉單在既有平倉確認流程中被拒或逾時未成交 → 5 秒後重新查證並再送，總共最多送出 3 次；送單前就失敗的嘗試（intent 落不了、該腿已有平倉單在途）同樣計一次並寫 `LEG_FLATTEN_ABORTED`（`by: "watchdog"`）；用完 → `Unknown("auto flatten failed 3 times")`。未知的通知與卡片依實際送出的單數撰寫（列出每張單與成交量）。
5. **Unknown 的處置**：寫 `NAKED_LEG_WATCHDOG`（含原因），`set_kill_switch_with_event(true, "NAKED_LEG_WATCHDOG_LOCK", …)`（Live 同時解除武裝），並通知；之後不再自動送單，直到使用者處理（人工確認已平倉或人工要求平倉）。
6. **適用範圍**：配對的執行器與目前模式一致（`executor_mismatch` 為否）；SIMULATION、EXCHANGE_DEMO、Live 皆適用。kill switch ON 時照常運作（平倉不開新倉）。
7. **頁面揭露**：風控頁固定區塊、人工處理卡與通知共用同一份文案產生函式（數字取實際的 `order_timeout_seconds`、緩衝 5 秒、重試 3 次），避免三處說法不一致。
8. **既有規格的配合**：`ui-trading-pages` 的 staged-orders-page 規格原本禁止顯示「暗示系統已自動補救」的文字；看門狗確實平倉後必須如實說明，因此同一 change 修改該句，加入此唯一例外（該 change 尚未封存，直接改其 delta）。頁面本身仍不提供或觸發自動平倉的按鈕。

## Risks / Trade-offs

- [通知機制尚未接上 macOS 系統通知（`LogNotifier` 只寫系統日誌）] → 文案一律寫「在 App 內顯示警示並記錄於系統日誌（尚未接上 macOS 系統通知）」，不寫「通知你」。
- [看門狗一輪的結果事件遺失（探測本身有 30 秒逾時，不再另設兜底）] → 探測一律由逾時包住並回報；任務本身 panic 時 `LostTaskGuard`（Drop）改送 `WatchdogProbe::unreadable` 算讀取失敗，手動 `FlattenLeg` 的讀取任務同理（清 `flatten_inflight` 並寫 `LEG_FLATTEN_ABORTED`），遺失只會發生在引擎已停止時。
- [另一腿其實晚到成交，系統已平掉一腿] → 先撤單並查證每張單都已終結、另一腿成交量 = 0 且持倉 = 0 才送；仍可能在毫秒級競態下發生，屆時變成反向單腿，下一輪查證會看到「另一腿有部位、這腿沒有」而再次處理，並留下事件。
- [交易所帳戶裡同幣種有其他部位（手動交易）] → `plan_leg_close` 的容差檢查會判定不符 → `Unknown`，不送單。
- [查詢 API 故障導致頻繁 Unknown 而鎖定] → 寧可鎖定；通知中寫明原因。
- [Demo 的 kill switch 被看門狗打開影響實驗] → 事件與通知寫明；使用者手動關閉即可。

## Migration Plan

純新增行為；預設啟用。回滾 = 還原此 change 的 commit。
