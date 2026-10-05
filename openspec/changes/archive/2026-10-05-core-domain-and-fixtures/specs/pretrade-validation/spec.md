## ADDED Requirements

### Requirement: 送單前檢查全部通過才算 PASS

送單前檢查 SHALL 是純函式，輸入為「剛抓到的最新資料」與生效中的風控設定，輸出為 PASS 或 BLOCK 並附上**所有**未通過檢查的具名清單。
任何一項未通過，整體 SHALL 為 BLOCK；系統 SHALL NOT 部分放行。
掃描當時暫存的資料 SHALL NOT 被直接用於送單判斷。

檢查項目 SHALL 包含：`DataFresh`、`CoinListed`、`ExchangeAllowed`、`NetEdgeQualified`、`PriceDrift`、`Liquidity`、`Margin`、`Leverage`、`ExistingExposure`、`RiskLimits`。

#### Scenario: 全部通過

- **WHEN** 所有最新資料皆在容許範圍內
- **THEN** 結果為 PASS，未通過清單為空

#### Scenario: 多項同時失敗時全部列出

- **WHEN** 價格漂移超限且保證金不足
- **THEN** 結果為 BLOCK，清單同時包含 `PriceDrift` 與 `Margin`

### Requirement: 價格漂移以送單前不久抓取的基準價比較

`PriceDrift` SHALL 將最新價格與「結算前約 15 秒重新抓取的基準價」比較；基準價不存在時才可退回掃描當時的價格。
漂移百分比嚴格大於 `max_price_drift_pct` 時為失敗，恰好等於則通過。long 與 short 兩腿 SHALL 各自檢查。

#### Scenario: 超過上限

- **WHEN** 基準價 100、最新價 100.06、`max_price_drift_pct` 為 0.05
- **THEN** `PriceDrift` 失敗

#### Scenario: 恰好等於上限

- **WHEN** 漂移恰為 0.05%
- **THEN** `PriceDrift` 通過

### Requirement: Liquidity 以 24 小時成交量判定且缺資料視為失敗

`Liquidity` SHALL 在任一腿的 24 小時成交量（以 USDT 計）小於生效的 `min_24h_volume_usdt` 時失敗；恰好等於門檻則通過。
任一腿缺少成交量資料時 SHALL 視為失敗（失敗即封閉），不得視為無限大或 0 以外的預設值。

#### Scenario: 一腿低於門檻

- **WHEN** `min_24h_volume_usdt` 為 50000，short 腿成交量為 49999.99
- **THEN** `Liquidity` 失敗

#### Scenario: 恰好等於門檻

- **WHEN** 一腿成交量恰為 50000
- **THEN** `Liquidity` 通過

#### Scenario: 缺少成交量資料

- **WHEN** 某腿沒有成交量資料
- **THEN** `Liquidity` 失敗

### Requirement: 無法計算漂移時 PriceDrift 失敗

基準價或最新價小於或等於 0 時，`PriceDrift` SHALL 失敗，SHALL NOT 略過該腿的檢查（Python 版在基準價為 0 時會略過而靜默通過）。

#### Scenario: 基準價為 0

- **WHEN** 某腿的基準價為 0
- **THEN** `PriceDrift` 失敗

### Requirement: 資料過期即阻擋

任一腿的價格或 funding 觀測，其 `observed_at` 距目前時間超過 `stale_data_threshold_ms` 時，`DataFresh` SHALL 失敗。
基準價與最新價 SHALL NOT 取自同一份未更新的快取。

#### Scenario: WebSocket 斷線導致價格過期

- **WHEN** 某腿最新價格的 `observed_at` 距目前 6 秒、門檻為 5000 毫秒
- **THEN** `DataFresh` 失敗，整體 BLOCK

### Requirement: 保證金、槓桿、持倉衝突與數量上限

下列四項檢查 SHALL 依各自的失敗條件判定：

- `Margin`：任一腿可用保證金小於所需保證金即失敗。
- `Leverage`：配對槓桿大於生效的 `max_leverage` 即失敗；系統 SHALL NOT 自動調降使用者設定後下單。
- `ExistingExposure`：任一腿的交易所上，該標的已存在不屬於本配對的持倉或未成交委託，即失敗。
- `RiskLimits`：已開啟配對數加 1 大於 `max_concurrent_pairs` 即失敗。

#### Scenario: 槓桿超過上限不自動調降

- **WHEN** 配對槓桿為 5、生效 `max_leverage` 為 4
- **THEN** `Leverage` 失敗，且不產生任何調整後的槓桿值

#### Scenario: 帳上已有同標的持倉

- **WHEN** Bybit 帳戶已有該標的、不屬於本配對的持倉
- **THEN** `ExistingExposure` 失敗

#### Scenario: 同時開啟配對數已達上限

- **WHEN** `max_concurrent_pairs` 為 3，且已有 3 組開啟中的配對
- **THEN** `RiskLimits` 失敗
