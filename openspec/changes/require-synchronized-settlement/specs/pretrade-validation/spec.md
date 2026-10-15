## MODIFIED Requirements

### Requirement: 送單前檢查全部通過才算 PASS

送單前檢查 SHALL 是純函式，輸入為「剛抓到的最新資料」與生效中的風控設定，輸出為 PASS 或 BLOCK 並附上**所有**未通過檢查的具名清單。
任何一項未通過，整體 SHALL 為 BLOCK；系統 SHALL NOT 部分放行。
掃描當時暫存的資料 SHALL NOT 被直接用於送單判斷。

檢查項目 SHALL 包含：`DataFresh`、`CoinListed`、`ExchangeAllowed`、`NetEdgeQualified`、`PriceDrift`、`Liquidity`、`Margin`、`Leverage`、`ExistingExposure`、`RiskLimits`。

`NetEdgeQualified` SHALL 以送單前剛抓到的最新資料判定，並與掃幣的達標判定一致：除了淨邊際與最低預期淨利達標之外，**兩腿最新的 `next_funding_time` 必須相同**（缺失視為不同）。已在交易單裡、建立時兩腿結算時間相同的配對，若送單前最新資料顯示兩腿結算時間已不同，SHALL 為 BLOCK，且清單包含 `NetEdgeQualified`。

#### Scenario: 全部通過

- **WHEN** 所有最新資料皆在容許範圍內
- **THEN** 結果為 PASS，未通過清單為空

#### Scenario: 多項同時失敗時全部列出

- **WHEN** 價格漂移超限且保證金不足
- **THEN** 結果為 BLOCK，清單同時包含 `PriceDrift` 與 `Margin`

#### Scenario: 送單前最新資料兩腿結算時間不同即 BLOCK

- **WHEN** 其餘檢查皆通過、淨邊際達標，但送單前最新資料中 long 腿下次結算為 08:00、short 腿為 04:00
- **THEN** 結果為 BLOCK，清單包含 `NetEdgeQualified`（且只有這一項）

#### Scenario: 送單前最新資料缺少結算時間即 BLOCK

- **WHEN** 其餘檢查皆通過、淨邊際達標，但一腿最新資料的 `next_funding_time` 缺失
- **THEN** 結果為 BLOCK，清單包含 `NetEdgeQualified`
