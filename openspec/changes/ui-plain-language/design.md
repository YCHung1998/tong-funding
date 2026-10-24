## Context

- `allowed_exchanges` 是風控設定；各頁目前不論是否允許都顯示三所，OKX 帳戶讀取失敗的原始訊息出現在總覽卡、交易單可用保證金列、持倉頁黃字、合約設定試算。
- 「上次執行結果」由 `vm/staged_orders.rs::last_results` 從不可變事件還原；`BLOCKED` 那筆的轉移發生在 `PRE_TRADE_CHECK`，事件 detail 為 `{"block": "pre-trade fetch failed: request timed out"}`，幣種要從配對資料取得而不是從腿的成交事件取得。
- 狀態名稱與錯誤碼目前散在各 view-model 直接格式化。

## Goals / Non-Goals

**Goals:** 停用的交易所不再製造雜訊但不隱藏曝險；畫面上不再出現英文內部代碼作為主要文字；上次執行結果可以一眼看出何時、什麼幣、結果與原因。

**Non-Goals:** 不停止輪詢停用的交易所（避免看不到該所既有持倉）；不改事件或日誌；不改狀態列（`loss-circuit-breaker`）；不改掃幣頁（`scanner-layout`）。

## Decisions

1. **單一對照模組**（`vm/labels.rs`）：`pair_state_label(&str) -> String`（中文名稱）、`pair_state_plain(PairState) -> &'static str`、`execution_mode_label`、`trigger_mode_label`、`pnl_status_label`（皆只回傳中文名稱，不帶說明句）、`exchange_error_label(&str) -> Option<(中文, 建議動作)>`（先收錄 `acct_mode_unsupported`）。未知代碼原樣顯示（不猜）。
2. **停用交易所**（審查修正：不能用「持倉是否為空」判斷）：單一規則在 `vm/disabled.rs`，所有頁面共用，結果三選一：`Quiet`（最新讀取成功無錯誤且不超過 `3 × ACCOUNT_POLL_MS`、`positions_incomplete` 為空、已儲存配對可讀取（`pair_records_error` 為空）、沒有持倉、沒有未結束配對有一腿在該所）→ 一行弱化文字，詳細文字附「N 秒前讀取」；`Unconfirmed`（讀取失敗／未連線／載入中／舊資料加最新讀取失敗／讀取過舊／配對紀錄讀取失敗，且沒有未結束配對）→ 一行警示色「<所> 已停用 · 無法確認持倉（原因）」，不寫「沒有持倉」；`Exposed`（有持倉、列表不完整或有未結束配對）→ 保留完整卡片／持倉列／保證金列與既有黃字紅字，加註 `DISABLED_WITH_EXPOSURE`。「未結束」= 非 `is_terminal()`（`FINALIZED`／`CANCELLED`／`BLOCKED`），引擎快照 `snap.pairs` 與 `snap.pair_records` 都看，同一 `pair_id` 只算一次；狀態讀不出來、已儲存紀錄的交易所未知，一律視為未結束（寧可多顯示）。`alerts.rs` 的帳戶警示對停用的 OKX 也在 `holds_exposure` 為真時照常產生，且只展開 `tradable_exchanges(identity)`（Live 不含 OKX，未標明交易所的紀錄不會讓 Live 出現 OKX 警示）。
   紅隊審查補強：帳戶輪詢回報「未連線」且有舊資料時（`live::failed_account_state`），保留舊資料為 `Loaded { error: Some("未連線：<原因>") }`（前綴 `NOT_CONNECTED_ERROR_PREFIX`），已知持倉不會因連線中斷而被忘記；`alerts.rs` 把這種 `Loaded` 仍視為「帳戶未連線」的斷線警示。`list_pairs()` 失敗時推送 `SourceUpdate::PairRecordsError`，`UiSnapshot.pair_records_error` 在下一次成功讀取時清除。總覽曝險摘要在有 `Unconfirmed` 時附加「（<交易所> 持倉無法確認）」、改用警示色，並把該所列入 `excluded_note`。合約設定頁只顯示報價，維持弱化單行。
3. **上次執行結果**：日期格式 `MM-DD HH:MM:SS UTC`；幣種取自配對；被擋下的原因取自轉移事件的 detail，先經白話對照（例如送單前重抓逾時）；淨損益取自最新 PnL 事件的 `breakdown.total.net`（沒有時不顯示）；訂單編號保留但改為次要文字（`ui-trading-pages` 規格要求顯示）。

## Risks / Trade-offs

- [白話對照與內部狀態不同步] → 對照函式窮舉 `PairState`（新增狀態時編譯或測試失敗）。
- [停用交易所的錯誤被藏起來，真的需要時看不到] → 一行文字可展開細節；只有「已確認沒有曝險」才弱化，無法確認時用警示色，有曝險時完整顯示。
- [風控頁允許在有未結束配對時取消勾選] → 以未結束配對判斷曝險，而不是只看持倉列表；代價是配對剛結束、帳戶尚未刷新時，多顯示一輪完整卡片。
