## MODIFIED Requirements

### Requirement: 送單前檢查全部通過才算 PASS

送單前檢查 SHALL 是純函式，輸入為「剛抓到的最新資料」與生效中的風控設定，輸出為 PASS 或 BLOCK 並附上**所有**未通過檢查的具名清單。
任何一項未通過，整體 SHALL 為 BLOCK；系統 SHALL NOT 部分放行。
掃描當時暫存的資料 SHALL NOT 被直接用於送單判斷。

檢查項目 SHALL 包含：`DataFresh`、`CoinListed`、`ExchangeAllowed`、`NetEdgeQualified`、`PriceDrift`、`Liquidity`、`Margin`、`Leverage`、`ExistingExposure`、`RiskLimits`。

`NetEdgeQualified` SHALL 只在下列兩者同時成立時通過，任一不成立即失敗（不新增第十一項檢查，十項清單維持不變）：
- 以最新資料計算的 Net Edge % ≥ 生效的 `net_edge_threshold_pct`；
- 以最新資料計算的預期淨收益 %（funding 收入 − 4 筆成交手續費 − 4 筆成交估計滑價，未扣安全邊際，除以每腿名目本金）≥ 生效的 `min_expected_net_pnl_pct`。恰好等於門檻則通過；名目本金不為正時視為失敗。

#### Scenario: 預期淨收益低於門檻

- **WHEN** Net Edge 已達 `net_edge_threshold_pct`，但預期淨收益 % 為 0.02、`min_expected_net_pnl_pct` 為 0.03
- **THEN** `NetEdgeQualified` 失敗，整體 BLOCK

#### Scenario: 全部通過

- **WHEN** 所有最新資料皆在容許範圍內
- **THEN** 結果為 PASS，未通過清單為空

#### Scenario: 多項同時失敗時全部列出

- **WHEN** 價格漂移超限且保證金不足
- **THEN** 結果為 BLOCK，清單同時包含 `PriceDrift` 與 `Margin`
