## ADDED Requirements

### Requirement: 提早平倉的最小有利幅度設定

全域風控設定 SHALL 新增選填欄位 `early_exit_min_move_pct`（百分比數值，0.05 代表 0.05%），驗證為 ≥ 0 的小數。此欄位 SHALL 沒有內建預設值：未填寫或為 0 時，單腿提早平倉 SHALL 停用，這不使設定被標為「不完整」，也不影響 Net Edge 與達標判定。

#### Scenario: 空白設定不啟用提早平倉

- **WHEN** 載入一份沒有 `early_exit_min_move_pct` 的設定
- **THEN** 提早平倉停用，設定的完整性判定不變

#### Scenario: 合法值被接受

- **WHEN** 把 `early_exit_min_move_pct` 設為 0.05
- **THEN** 驗證通過，值以 Decimal 保存，不做四捨五入

#### Scenario: 負值被拒絕

- **WHEN** 嘗試把 `early_exit_min_move_pct` 設為 −0.01
- **THEN** 驗證失敗並回傳欄位名稱，設定不被改動
