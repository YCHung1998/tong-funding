## MODIFIED Requirements

### Requirement: 達標判定

一個機會 SHALL 僅在下列條件全部成立時為「達標」：`net_edge_pct ≥ net_edge_threshold_pct`；
**兩腿的 `next_funding_time` 相同（兩腿都在同一個結算時間 `T` 結算）；任一腿的 `next_funding_time` 缺失（為 0 或沒有資料）SHALL 視為不相同**；
兩腿的 `data_status` 皆為 `LISTED` 且各自帶有有效的 funding 週期（以 struct 欄位或反序列化繞過建構子產生的「LISTED 但無週期」觀測 SHALL 視為不達標）；兩腿的 `volume_24h_quote` 皆不小於 `min_24h_volume_usdt`；
兩個交易所皆在 `allowed_exchanges`；若 `allowed_coins` 非空，該標的在其中。
缺少成交量資料 SHALL 視為 0（失敗即封閉），不得視為無限大。

兩腿結算時間不同時，Net Edge 的**計算**仍依「Net Edge 採單次結算模型」進行（供顯示），但該機會 SHALL NOT 達標，不論 `net_edge_pct` 多高。

#### Scenario: 任一腿成交量不足即不達標

- **WHEN** 一腿成交量為 `min_24h_volume_usdt` 以上、另一腿低於，且 Net Edge 達門檻
- **THEN** 不達標

#### Scenario: 成交量缺失視為不足

- **WHEN** 某腿沒有成交量資料
- **THEN** 不達標

#### Scenario: Gross Spread 不影響達標

- **WHEN** gross spread 很大但 `net_edge_pct` 低於門檻
- **THEN** 不達標

#### Scenario: 兩腿結算時間不同即使 Net Edge 很高也不達標

- **WHEN** long 在 Bybit（下次結算 08:00）、short 在 Binance（下次結算 04:00），只有 short 腿會結算，`net_edge_pct` 遠高於 `net_edge_threshold_pct`，其餘條件皆成立
- **THEN** 不達標，但 Net Edge 仍依單次結算模型算出（只計入 short 腿）

#### Scenario: 兩腿結算時間相同且 Net Edge 達門檻才達標

- **WHEN** 兩腿下次結算皆為 08:00，`net_edge_pct` 達門檻，其餘條件皆成立
- **THEN** 達標

#### Scenario: 結算時間缺失視為不同步

- **WHEN** 一腿的 `next_funding_time` 為 0 或沒有資料，另一腿為 08:00，其餘條件皆成立且 Net Edge 達門檻
- **THEN** 不達標

#### Scenario: 兩個方向都不同步時兩個方向都不達標

- **WHEN** 同一標的在兩個交易所的下次結算時間不同
- **THEN** 無論以哪一個交易所作為 long，結果皆為不達標，多空方向的選擇不改變此結果
