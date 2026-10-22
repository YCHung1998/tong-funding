## ADDED Requirements

### Requirement: 機會型態分類

系統 SHALL 對每個機會（已指定 long、short 的配對）計算：

- `hourly_spread_pct_per_hour` = `(r_S ÷ h_S − r_L ÷ h_L) × 100`，其中 `r` 為費率（小數）、`h` 為該腿結算週期的小時數。
- 費率差收入 `rate_gap_income_pct` = `hourly_spread_pct_per_hour × min(h_L, h_S)`（兩腿都完整累積的期間內，每小時費率差能貢獻的收入）。
- 模型收入 `model_income_pct` = `net-edge` spec 的預期 funding 收入 ÷ N × 100。

型態 SHALL 為：

- `None`：`model_income_pct ≤ 0`。
- `IntervalMismatch`（週期錯位型）：兩腿週期不同，且 `rate_gap_income_pct < 0.5 × model_income_pct`（一半以上的收入來自週期不同，而非每小時費率差）。
- `RateGap`（費率差型）：其他情況（含兩腿週期相同）。

此分類 SHALL 只用於記錄與顯示，SHALL NOT 影響達標判定、方向選擇或下單。

#### Scenario: 週期錯位型

- **WHEN** long 為 Bybit（週期 8h，`r_L = −0.007337`），short 為 Binance（週期 4h，`r_S = −0.003346`）
- **THEN** 模型收入為 0.3991%，每小時費率差約為 0.0081 %/h，費率差收入約為 0.0322%，型態為 `IntervalMismatch`

#### Scenario: 費率差型（週期相同）

- **WHEN** 兩腿週期皆為 8h，`r_L = −0.002`、`r_S = +0.001`
- **THEN** 型態為 `RateGap`

#### Scenario: 週期不同但費率差也大

- **WHEN** long 為 8h 週期 `r_L = −0.0040`，short 為 4h 週期 `r_S = +0.0010`
- **THEN** 模型收入為 0.5000%，每小時費率差為 0.0750 %/h，費率差收入為 0.3000%（≥ 0.2500），型態為 `RateGap`

#### Scenario: 沒有收入

- **WHEN** 預期 funding 收入為 0 或負
- **THEN** 型態為 `None`，每小時費率差仍輸出

### Requirement: 型態寫入進場快照

進場檢查通過時寫入的 `entry_snapshot.net_edge` SHALL 增加 `edge_kind` 與 `hourly_spread_pct_per_hour`；既有欄位與值不變。

#### Scenario: 快照包含型態

- **WHEN** 配對通過送單前檢查並寫入進場快照
- **THEN** `net_edge` 內含 `edge_kind`（`IntervalMismatch`、`RateGap` 或 `None`）與 `hourly_spread_pct_per_hour`
