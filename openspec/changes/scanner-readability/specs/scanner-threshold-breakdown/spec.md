## ADDED Requirements

### Requirement: 掃幣頁頂端顯示達標所需價差的公式展開

掃幣頁頂端 SHALL 對每一組啟用的交易所組合（例如 Binance↔Bybit）顯示一行公式，以該組合合併覆寫後的實際設定值計算：

`所需費率價差 % = 門檻 + 2 × (taker_fee_L + taker_fee_S) + 4 × est_slippage + safety_margin = 結果`

每一項 SHALL 顯示數值並標註其風控欄位名稱（`net_edge_threshold_pct`、`taker_fee_pct`、`est_slippage_pct`、`safety_margin_pct`）；所有數值以百分比數值顯示到小數第 4 位。
「所需費率價差」SHALL 定義為 `(−r_L × settles_L + r_S × settles_S) × 100` 需達到的最小值，與 `net-edge` spec 的 Net Edge 公式等價（`net_edge_pct ≥ 門檻` ⇔ 費率價差 ≥ 結果）。
區塊 SHALL 提供前往風控設定頁的連結。

#### Scenario: 一般組合

- **WHEN** Binance↔Bybit 的合併設定為：門檻 0.05、Binance taker 0.05、Bybit taker 0.055、滑價 0.02、安全邊際 0.01
- **THEN** 該行顯示 `所需費率價差 % = 0.0500 + 2×(0.0500+0.0550) + 4×0.0200 + 0.0100 = 0.3500`

#### Scenario: 每腿覆寫生效

- **WHEN** 全域門檻為 0.05，但 Bybit 覆寫門檻為 0.08
- **THEN** 含 Bybit 的組合以 0.0800 作為門檻項，不含 Bybit 的組合仍為 0.0500

### Requirement: 較嚴的最低淨利條件一併列出

若某組合的 `min_expected_net_pnl_pct + 手續費 + 滑價` 大於上述結果，該行 SHALL 另列
`min_expected_net_pnl + 2 × (taker_fee_L + taker_fee_S) + 4 × est_slippage = 結果2`，並標示「以較嚴者為準：結果2」。

#### Scenario: 最低淨利較嚴

- **WHEN** 門檻 0.01、安全邊際 0.01、`min_expected_net_pnl_pct` 0.05，其餘同上
- **THEN** 該行顯示兩條式子，並標示以第二條為準

### Requirement: 設定缺失時說明缺少的欄位

組合缺少必填設定（門檻、滑價、任一腿 taker fee）或風控設定讀取失敗時，該行 SHALL 顯示「無法計算：缺少 <欄位清單>」，SHALL NOT 以 0 代入計算。

#### Scenario: 缺少 Bybit 手續費

- **WHEN** Bybit 的 `taker_fee_pct` 未設定
- **THEN** 含 Bybit 的組合顯示「無法計算：缺少 Bybit taker_fee_pct」

### Requirement: 掃幣表格奇偶列底色交錯

掃幣表格的奇數列與偶數列 SHALL 以兩個可辨識的不同底色繪製（斑馬紋），滑鼠移過的列 SHALL 另有 hover 底色；文字在兩種底色上的對比度 SHALL 不低於 4.5:1。

#### Scenario: 交錯可見

- **WHEN** 掃幣表格顯示 10 列
- **THEN** 第 1、3、5… 列與第 2、4、6… 列的實際繪製底色不同
