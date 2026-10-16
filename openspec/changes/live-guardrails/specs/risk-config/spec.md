## ADDED Requirements

### Requirement: Live 專用上限欄位

全域風控設定 SHALL 新增 live 專用欄位：`live_max_leg_notional_usdt`（每腿名目上限，正的 Decimal）與 `live_max_pairs_per_arming`（一次武裝最多進場配對數，正整數）。兩者 SHALL 沒有內建預設值，未填寫時設定在 Live 身分下被標為「不完整」，預檢不通過；Demo 身分 SHALL 忽略這兩個欄位。程式 SHALL 內建絕對天花板（本版：每腿名目 100 USDT、每次武裝 5 筆），設定超過天花板 SHALL 驗證失敗並回報欄位名稱。

#### Scenario: Live 未填上限即不完整

- **WHEN** Live 身分載入一份沒有 `live_max_leg_notional_usdt` 的設定
- **THEN** 設定被標為不完整，預檢不通過

#### Scenario: 超過天花板被拒

- **WHEN** 設定 `live_max_pairs_per_arming = 9`
- **THEN** 驗證失敗並指出欄位與天花板

#### Scenario: Demo 不受影響

- **WHEN** Demo 身分載入沒有這兩個欄位的設定
- **THEN** 設定完整性判定不變
