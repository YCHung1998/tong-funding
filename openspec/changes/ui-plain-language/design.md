## Context

- `allowed_exchanges` 是風控設定；各頁目前不論是否允許都顯示三所，OKX 帳戶讀取失敗的原始訊息出現在總覽卡、交易單可用保證金列、持倉頁黃字、合約設定試算。
- 「上次執行結果」由 `vm/staged_orders.rs::last_results` 從不可變事件還原；`BLOCKED` 那筆的轉移發生在 `PRE_TRADE_CHECK`，事件 detail 為 `{"block": "pre-trade fetch failed: request timed out"}`，幣種要從配對資料取得而不是從腿的成交事件取得。
- 狀態名稱與錯誤碼目前散在各 view-model 直接格式化。

## Goals / Non-Goals

**Goals:** 停用的交易所不再製造雜訊但不隱藏曝險；畫面上不再出現英文內部代碼作為主要文字；上次執行結果可以一眼看出何時、什麼幣、結果與原因。

**Non-Goals:** 不停止輪詢停用的交易所（避免看不到該所既有持倉）；不改事件或日誌；不改狀態列（`loss-circuit-breaker`）；不改掃幣頁（`scanner-layout`）。

## Decisions

1. **單一對照模組**（`vm/labels.rs`）：`pair_state_label(&str) -> (中文, 說明)`、`execution_mode_label`、`trigger_mode_label`、`pnl_status_label`、`exchange_error_label(&str) -> Option<(中文, 建議動作)>`（先收錄 `acct_mode_unsupported`）。未知代碼原樣顯示（不猜）。
2. **停用交易所**：由 view-model 依目前風控設定判斷；有持倉時顯示持倉列並加註「此交易所已停用，仍有持倉」。
3. **上次執行結果**：日期格式 `MM-DD HH:MM:SS UTC`；幣種取自配對；被擋下的原因取自轉移事件的 detail，先經白話對照（例如送單前重抓逾時）；淨損益取自最新 PnL 事件的 `breakdown.total.net`（沒有時不顯示）；訂單編號保留但改為次要文字（`ui-trading-pages` 規格要求顯示）。

## Risks / Trade-offs

- [白話對照與內部狀態不同步] → 對照函式窮舉 `PairState`（新增狀態時編譯或測試失敗）。
- [停用交易所的錯誤被藏起來，真的需要時看不到] → 一行弱化文字可展開細節。
