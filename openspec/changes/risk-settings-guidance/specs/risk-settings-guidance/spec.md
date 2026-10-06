## ADDED Requirements

### Requirement: 風控欄位標示嚴格方向

風控設定頁（全域與每腿覆寫）的每個數值欄位 SHALL 在欄位名稱前顯示方向標記，分三類：
- 「▲ 越高越嚴」：`net_edge_threshold_pct`、`min_expected_net_pnl_pct`、`safety_margin_pct`、`est_slippage_pct`、`min_24h_volume_usdt`
- 「▼ 越低越嚴」：`max_leverage`、`max_price_drift_pct`、`stale_data_threshold_ms`、`order_timeout_seconds`、`max_leg_imbalance_pct`、`max_concurrent_pairs`
- 「● 事實值」：各交易所 `taker_fee_pct`（說明「請依帳戶手續費等級實填」）
「越高越嚴」與「越低越嚴」SHALL 以兩種不同顏色的方框呈現，且 SHALL 同時顯示箭頭文字，不得只靠顏色區分。

#### Scenario: 門檻標為越高越嚴

- **WHEN** 開啟風控設定頁
- **THEN** `net_edge_threshold_pct` 欄位前顯示「▲ 越高越嚴」方框

#### Scenario: 槓桿標為越低越嚴

- **WHEN** 開啟風控設定頁
- **THEN** `max_leverage` 欄位前顯示「▼ 越低越嚴」方框，顏色與「▲」方框不同

#### Scenario: 手續費標為事實值

- **WHEN** 開啟風控設定頁
- **THEN** 每個 `taker_fee_pct` 欄位前顯示「● 事實值」

### Requirement: 方向標記與保守合併規則一致

對每個可覆寫欄位，標記為「越高越嚴」者 SHALL 恰為 `effective_for_pair` 取較大值的欄位，標記為「越低越嚴」者 SHALL 恰為取較小值的欄位。

#### Scenario: 一致性測試

- **WHEN** 對每個可覆寫欄位，以兩腿覆寫值 1 與 2 呼叫 `effective_for_pair`
- **THEN** 結果為 2 的欄位全標「越高越嚴」，結果為 1 的欄位全標「越低越嚴」

### Requirement: 參數說明視窗

「風控設定 Risk Management」標題旁 SHALL 有 `?` 按鈕；點擊 SHALL 開啟說明視窗，視窗可關閉。
視窗 SHALL 對每個數值欄位列出：欄位名、方向標記、意義、所在公式（若有）、影響的判斷（達標、送單前檢查、執行）。
視窗 SHALL 包含：
- `net_edge_pct = 費率價差 − 2 × (taker_fee_L + taker_fee_S) − 4 × est_slippage − safety_margin`
- 達標條件（Net Edge ≥ 門檻、扣安全邊際前淨利 ≥ `min_expected_net_pnl_pct`、兩腿成交量 ≥ `min_24h_volume_usdt`、交易所與幣種在允許清單）
- `所需費率價差 = 門檻 + 2 × (taker_fee_L + taker_fee_S) + 4 × est_slippage + safety_margin`
- 每腿覆寫取兩腿與全域中較嚴者的說明

#### Scenario: 開啟說明

- **WHEN** 使用者點擊 `?`
- **THEN** 出現說明視窗，含上述公式與每個欄位的說明

#### Scenario: 每個欄位都有說明

- **WHEN** 列舉全域與覆寫欄位
- **THEN** 每個欄位的說明文字非空
