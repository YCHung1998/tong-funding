## ADDED Requirements

### Requirement: 提早平倉的最小有利幅度設定

全域風控設定 SHALL 新增欄位 `early_exit_min_move_pct`（百分比數值，0.3 代表 0.3%），驗證為 ≥ 0 的小數，**預設值為 0.3**（使用者 2026-10-15 拍板）。設為 0 時，單腿提早平倉 SHALL 停用。此欄位不使設定被標為「不完整」，也不影響 Net Edge 與達標判定。已存在、沒有此欄位的設定在載入時 SHALL 取得預設值 0.3。

#### Scenario: 沒有此欄位的既有設定取得預設 0.3

- **WHEN** 載入一份沒有 `early_exit_min_move_pct` 的既有設定
- **THEN** 該欄位為 0.3，提早平倉啟用，設定的完整性判定不變

#### Scenario: 設為 0 停用提早平倉

- **WHEN** 把 `early_exit_min_move_pct` 設為 0
- **THEN** 提早平倉停用，行為與沒有此功能時相同

#### Scenario: 合法值被接受

- **WHEN** 把 `early_exit_min_move_pct` 設為 0.05
- **THEN** 驗證通過，值以 Decimal 保存，不做四捨五入

#### Scenario: 負值被拒絕

- **WHEN** 嘗試把 `early_exit_min_move_pct` 設為 −0.01
- **THEN** 驗證失敗並回傳欄位名稱，設定不被改動
