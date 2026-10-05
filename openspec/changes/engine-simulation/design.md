## Context

來源：`mvp-python`（HANDOFF.md 為準）與 2026-10-05 的抗辯結論。以下是**已對照原始碼確認**的 Python 版事實，設計依此展開：

| 事實 | 來源 |
|---|---|
| `execution_mode` 為 SIMULATION 時，Node 0 通過後直接把配對標為 `FINALIZED` 並記 `ENTRY_SIMULATED`，不經成交、不經出場 | `auto_scheduler.py:_execute_entry` |
| 進場觸發為 `now >= entry_trigger_ms`，**沒有上限**：程式停機後重啟、已過結算也會進場 | `auto_scheduler.py:select_due_entries` |
| 基準價在 `entry_trigger_ms` 之前 15 秒（`PRICE_REFRESH_LEAD_MS`）抓取，而 `entry_trigger_ms` 本身是結算前 15 秒，所以基準價實際約在結算前 30 秒 | `auto_scheduler.py` 常數與 `auto_stage.py:ENTRY_OFFSET_MS` |
| 排程器只讀 `rc.load_risk_config()["global"]`，從不讀每腿覆寫；`order_timeout_seconds` 沒有任何執行路徑讀它 | `auto_scheduler.py`、HANDOFF Fragility #1 |
| kill switch 停機時，tick 在 `is_halted()` 之後直接 `return`：**自動進場與自動出場都被擋**；`_execute_close` 開頭也再檢查一次 | `auto_scheduler.py:tick`、`_execute_close` |
| PREPARED 自動撤銷只在 `trigger_mode == AUTO` 時執行，且在 kill switch 檢查之前，所以停機時仍執行 | `auto_scheduler.py:tick` |
| `_get_latest_price` 對 Binance 先取 WS 快取，只要該標的在快取裡就直接回傳，不檢查連線狀態與時間戳 | `app.py:_get_latest_price` |
| `has_symbol_pending()` + `add()` 是兩次獨立的鎖，非原子 | `order_queue_store.py`、HANDOFF Fragility #2 |
| `submit_legs` 只捕捉三種 `*ApiError`；`requests` 的逾時與斷線例外會一路往上拋，使配對卡在 `ORDER_SUBMIT`，且已成功的那一腿訂單仍在交易所上 | `trade_pipeline.py:submit_legs`、`auto_scheduler.py:_run_forever` |
| 兩支 client 都沒有帶 client order id | `binance_client.py`、`bybit_client.py` |
| `app.py` 把 `LIVE` 當成可選的 `execution_mode`；手動下單頁不受 `execution_mode` 約束 | `app.py` 第 1681–1687 行附近、`HANDOFF.md` Safety invariants |

`core` 提供純邏輯（`next()`、送單前檢查、`effective_for_pair`、`Quantity`）；`store` 提供原子新增、`order_intents`、kill switch 持久化與失敗即封閉；
`exchange-readonly-adapters` 提供唯讀的行情、餘額、持倉、委託與 `serverTime` 偏移。本 change 不新增任何對交易所的寫入能力。

## Goals / Non-Goals

**Goals**
- 在 SIMULATION 下把排程、Node 0–8、kill switch、恢復流程完整跑通，且全部可用假時鐘與假資料來源測試。
- 把崩潰安全（先落地意圖、重啟對帳、對帳前不開倉）做進引擎，而不是留給真實下單階段。
- 讓 `exchange-demo-execution` 只需要實作 `Executor`，不必改動流程邏輯。

**Non-Goals**
- 不實作任何簽名下單、撤單或平倉請求（`exchange-demo-execution`）。
- 不做 UI 頁面與橫幅（`ui-trading-pages`）；本 change 只產生 Snapshot 與 Command 的契約。
- 不做 Funding PnL（`funding-pnl`）。
- 不自動修復單腿失敗或不平衡（使用者拍板為完全人工）。

## Decisions

**D1　引擎是單一 actor，不用共享鎖。**
Python 版的競態都來自「多個執行緒各拿一把鎖操作同一份狀態」（HANDOFF Fragility #2、#3）。單一 actor 讓序列化是結構性的，原子性不需要靠每個函式記得上鎖。
代價：actor 內的任何慢操作都會卡住全部配對，所以 D3 規定 I/O 不得在 actor 內 `await`。

**D2　Command / Event / Snapshot 三分。**
`Command` 是外部（UI、排程器的 tick）要求的動作；`Event` 是 spawn 的 I/O task 回送的結果；`Snapshot` 是唯讀複本。排程器的 tick 與自動進場也走 `Command` 通道（內部發送者），所以 kill switch 與 `opens_exposure()` 只需要在一個入口檢查。

**D3　I/O 以 spawn task 執行，行情走獨立 `watch`。**
下單呼叫可能逾時（HTTP 逾時預設值未驗證，Python 版為 10 秒），若在 actor 內 `await` 會讓全部配對的 tick 停擺。行情用 `watch` 只保留最新值，天然合併高頻更新，不需要有界佇列的丟棄策略。Snapshot 最小間隔暫定 250 毫秒（**我的提議，未驗證**，見 Open Questions）。

**D4　先落地再生效。**
每次轉移與其意圖先寫 store，成功後才執行動作。理由：崩潰時，資料庫裡的狀態一定不落後於已對交易所發出的動作。代價：每筆送單前多一次寫入的延遲（SQLite 單次寫入的延遲未量測，**未驗證**，需在 task 2.3 或 4.1 實測並記錄）。

**D5　`opens_exposure()` 的分類。**

| Command | opens_exposure | 理由 |
|---|---|---|
| 排程器內部進場觸發、手動執行進場 | 真 | 開新倉 |
| 手動下單 | `!reduce_only` | 無 reduce-only 的訂單可能開倉或加倉；停機時仍可手動減倉（使用者確認：優先清倉拿回現金） |
| 新增 PREPARED 配對 | 假 | 尚未下任何單；進場觸發時才檢查 kill switch |
| 自動出場、手動出場、人工平倉、人工確認已平倉 | 假 | 只減少曝險 |
| 取消 PREPARED、修改 `trigger_mode`、修改 `execution_mode`、修改設定、切換 kill switch | 假 | 不產生曝險 |

`match` 不含萬用分支。若日後用 `clippy::wildcard_enum_match_arm` 守住，須先驗證該 lint 在本專案設定下確實會對遺漏報錯（**未驗證**），否則改用編譯失敗測試。

**D6　SIMULATION 的結構保證用「工廠 + 無 client 的型別」雙層。**
- 型別層：`SimulatedExecutor` 沒有 client 欄位，所在模組不引用 `exchange` 內具下單能力的型別。
- 生命週期層：能下單的 `Executor` 只由注入的「真實執行器工廠」在切換至 `EXCHANGE_DEMO` 時建立，切回 `SIMULATION` 即丟棄；`SIMULATION` 期間進程內沒有能下單的實例。測試把工廠換成「呼叫就計數」的版本，整輪 SIMULATION 後斷言為 0。
- 依賴層：`exchange-readonly-adapters` 只有 GET，本 change 合併時 build 內根本沒有下單程式碼；`exchange-demo-execution` 合併後則由上兩層守住。
理由：只靠「模式旗標 + if」是 Python 版的做法，一個 bug 就能繞過；這裡讓繞過需要同時破壞型別、工廠與測試。

**D7　SIMULATION 走完整流程，包含出場與已平倉確認。**
Python 版在進場後就 `FINALIZED`（`ENTRY_SIMULATED`），所以從未在模擬下測過 Node 4–8。新版 `SimulatedExecutor` 維護模擬的持倉與委託帳，使狀態機、對帳與已平倉確認都能被測。預設成交價為注入的最新價格、完整成交、無手續費與滑價（不宣稱真實；Funding PnL 不由模擬決定）。

**D8　`Executor` 與 `AccountView` 分離。**
`Executor` 只管訂單生命週期（送單、撤單、依 `client_order_id` 查單）；`AccountView` 管持倉、未成交委託、餘額（來自 `exchange-readonly-adapters` 的唯讀實作）。SIMULATION 下 `AccountView` 的持倉與委託來自模擬帳，餘額來源見 Open Questions。這樣唯讀能力不被誤算成「能下單」。

**D9　進場視窗 `[T − entry_lead, T)`，錯過先警示再取消；時點全部是設定值。**
Python 版 `now >= entry_trigger_ms` 沒有上限，重啟後可能在結算後進場，拿不到該次 funding 卻付了四筆手續費。新版加上上限（使用者 2026-10-05 確認）。
時點集中在 `EngineTimings`（不寫死在流程裡，使用者要求之後好調整）：
- `entry_lead_ms`：預設 **10,000（T−10）**。使用者希望 T−5，但送單延遲尚未實測（重抓單一標的實測 0.16–0.26 秒，每所僅 3 次；送單延遲需 demo 金鑰）。
  **改為 T−5 的條件**：`exchange-demo-execution` 實測「進場觸發 → 兩腿皆被交易所接受」p99 < 2,500 ms（5 秒的一半）。
- `base_price_lead_ms`：基準價在進場時點之前多久抓取，預設 5,000（即 T−15，與使用者最初描述一致）。
- `exit_delay_ms`：預設 15,000（T+15）。
- `missed_window_policy`：目前只有 `WarnThenCancel`——錯過視窗時先寫一筆警示事件 `ENTRY_WINDOW_MISSED`，再轉 `CANCELLED`。以 enum 表示，日後新增策略只需加變體。
出場沒有上限（晚平倉比不平倉好）。

**D10　結算時間 `T` 在配對建立時固定。**
`T` 由兩腿 `next_funding_time` 取較早者（core 規則），建立時保存。Node 0 重新抓取後若發現 funding 時間或週期已變，目前由 `NetEdgeQualified` 以最新資料重算來間接反映；是否要另設「結算時間改變即 BLOCK」未決（Open Questions）。

**D11　`client_order_id` 決定性產生，格式取保守交集。**
由「配對 uuid 前綴、腿、動作、序號」組成並加上 `sim` 或 `demo` 前綴，只用 `[A-Za-z0-9_-]`、長度 ≤ 36。上限 36 與字元集是**依我對各所公開文件的記憶取的保守值，未驗證**，須在 `exchange-demo-execution` 的 demo 驗證中確認各所都接受。決定性使重啟後可由配對資料重算 id，也使「結果未知時以原 id 查詢」成立。

**D12　重啟對帳是唯讀的，對不上就交給人。**
決策是完全人工，所以對帳只查詢、轉狀態、警示，不重送、不撤單、不平倉。規則表見 `crash-recovery` spec。「查不到意圖」是否代表「從未送達」取決於交易所查單的保留期限與延遲，**未驗證**；因此只有在持倉與委託皆無曝險時才把它判為未送達，否則進 `UNRESOLVED`。

**D13　已開啟配對包含警示中的配對。**
`PARTIAL_FAILURE`、`IMBALANCED`、`UNRESOLVED` 的配對實際上仍有曝險，必須佔用 `max_concurrent_pairs`，否則系統會在有未處理單腿時繼續開新倉。

## 與 Python 版的差異

| 項目 | Python 版 | Rust 版 | 原因 |
|---|---|---|---|
| SIMULATION 後續 | 進場後直接 `FINALIZED` | 走完整輪 | D7 |
| 進場時點上限 | 無 | `T` 之前 | D9 |
| 進場時點 / 基準價 | 進場 T−15；基準價再提前 15 秒（約 T−30） | 進場 T−10、基準價 T−15，皆為設定值 | 使用者決定（D9） |
| 每腿覆寫 | 只存不讀 | `effective_for_pair` 接進 Node 0 與逾時 | 使用者決定 |
| `order_timeout_seconds` | 只在設定頁 | 執行路徑讀取 | 使用者決定 |
| 停機時自動出場 | 被擋 | 不被擋（`opens_exposure` 為假） | 使用者 2026-10-05 確認 |
| 送單意圖 | 無 | 先落地、帶 `client_order_id` | 抗辯 |
| 手動下單頁 | 不受 `execution_mode` 約束 | 受約束，同一條路徑 | 使用者決定 |
| `requests` 類例外 | 往上拋、配對卡住 | 明確的「結果未知」狀態 | 見 `exchange-demo-execution` |

## Risks / Trade-offs

- **單一 actor 是單點。** actor 內任何 panic 會讓整個引擎停止。需要在 task 1.2 決定 panic 時的行為（預期：進入停機並顯示原因，不自動重啟），**未驗證**。
- **每次轉移先寫 store 增加延遲。** 進場時點前後是對延遲敏感的窗口（兩腿送單越晚越可能錯過結算）；SQLite 寫入延遲**未量測**。
- **Snapshot 限頻與停機狀態可見性。** 若 UI 最小間隔過大，使用者按下 kill switch 後看到的狀態可能延遲；停機的 Command 回應不走限頻，需要在 UI change 注意。
- **對帳假設交易所能以 `client_order_id` 查單。** 若某所對舊訂單查不到，會有更多配對落入 `UNRESOLVED`，增加人工負擔；實際保留期限**未驗證**。
- **SIMULATION 的模擬成交過於樂觀**（完整成交、無滑價），不能用來估計單腿失敗頻率；該統計只能在 `exchange-demo-execution` 取得。

## 已決定（2026-10-05，使用者）

1. **core 轉移已涵蓋。** 已對照 `core/src/pair.rs` 的 `next()`：`PREPARED → CANCELLED`、`ORDER_SUBMIT`/`FILL_MONITOR`/`CLOSING` 的 `RestartFoundPartial → PARTIAL_FAILURE` 與 `RestartUndetermined → UNRESOLVED`、`CLOSING` 的 `CloseFailed → PARTIAL_FAILURE` 皆存在，不需修改 core。
2. **停機時自動出場照常執行。**
3. **進場 T−10、基準價 T−15**，皆為設定值；改 T−5 的條件見 D9。
4. **進場視窗上限採用**；錯過時先寫警示事件再取消；策略做成可調整（D9）。
5. **手動下單以 `reduce_only` 分類**（D5）。
6. **SIMULATION 的保證金**：讀交易所 demo 帳戶的真實餘額（唯讀簽名 GET，經 `AccountView`）。金鑰在使用者 Mac 的 Keychain；讀不到餘額時 `Margin` 檢查失敗（BLOCK，失敗即封閉），不使用預設值或虛擬餘額。測試一律用假的 `AccountView`。
7. **SIMULATION 中斷後的配對一律 `UNRESOLVED`。**

## Open Questions

- Snapshot 最小間隔 250 毫秒、`client_order_id` 長度 36 與字元集、交易所查單保留期限、SQLite 寫入延遲、actor panic 行為皆**未驗證**，分別在 task 1.2、4.1、`exchange-demo-execution` 的驗證中確認。
- Node 0 發現結算時間或週期改變時是否另行 BLOCK（D10），目前不另設檢查。
