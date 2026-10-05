# net-edge Specification

## Purpose
TBD - created by archiving change core-domain-and-fixtures. Update Purpose after archive.
## Requirements
### Requirement: Net Edge 採單次結算模型

系統 SHALL 以「進場後只經歷一次結算（時間 `T`，見 `funding-observation`）」計算 Net Edge。
**單位約定**：交易所回報的 funding rate（`r_L`、`r_S`）為小數（0.0001 代表 0.01%）；
所有以 `_pct` 結尾的設定值（`taker_fee_pct`、`est_slippage_pct`、`safety_margin_pct`、`net_edge_threshold_pct`、`max_price_drift_pct`）為**百分比數值**（0.01 代表 0.01%），
計算時 SHALL 先除以 100 再使用。`net_edge_pct` 同樣以百分比數值表示。

令 `N` 為每腿目標名目本金（USDT），long 腿 rate 為 `r_L`、short 腿 rate 為 `r_S`，
`settles_L`、`settles_S` 為各腿是否在 `T` 結算（0 或 1），則：

- 預期 funding 收入 = `N × ( −r_L × settles_L + r_S × settles_S )`
  （rate 為正時 long 付錢、short 收錢；rate 為負時相反）
- 手續費 = `N × 2 × (taker_fee_pct_L + taker_fee_pct_S) ÷ 100`（兩腿各開倉與平倉共四筆成交）
- 滑價成本 = `N × 4 × est_slippage_pct ÷ 100`（四筆成交各估一次）
- 安全邊際 = `N × safety_margin_pct ÷ 100`
- `net_edge_usdt` = 預期 funding 收入 − 手續費 − 滑價成本 − 安全邊際
- `net_edge_pct` = `net_edge_usdt ÷ N × 100`

所有計算 SHALL 使用 Decimal，且 SHALL NOT 在 core 內做顯示用的四捨五入。

#### Scenario: 兩腿同時結算且 funding 為正

- **WHEN** `r_L = −0.0001`、`r_S = +0.0003`，兩腿皆在 `T` 結算，`taker_fee_pct_L = taker_fee_pct_S = 0.02`，`est_slippage_pct = 0`，`safety_margin_pct = 0`，`N = 1000`
- **THEN** 預期 funding 收入為 0.4 USDT、手續費為 0.8 USDT、`net_edge_usdt` 為 −0.4 USDT、`net_edge_pct` 為 −0.04

#### Scenario: 安全邊際以百分比數值計算

- **WHEN** 其餘成本皆為 0、funding 收入為 0.4 USDT、`safety_margin_pct = 0.01`、`N = 1000`
- **THEN** 安全邊際為 0.1 USDT（1000 × 0.01 ÷ 100），`net_edge_usdt` 為 0.3 USDT

#### Scenario: 週期不同時只計入會結算的腿

- **WHEN** long 在 Bybit（`r_L = 0`，8 小時週期，較晚結算），short 在 Binance（`r_S = +0.0005`，4 小時週期，較早結算），其餘成本為 0，`N = 1000`
- **THEN** 只有 short 腿結算，預期 funding 收入為 0.5 USDT，而不是以價差 0.0005 乘上兩腿

#### Scenario: 方向相反的 rate 仍正確計算

- **WHEN** long 腿 rate 為 +0.0002（long 要付錢）、short 腿 rate 為 +0.0002，兩腿皆結算，成本為 0，`N = 1000`
- **THEN** 預期 funding 收入為 0（long 付 0.2、short 收 0.2）

### Requirement: 達標判定

一個機會 SHALL 僅在下列條件全部成立時為「達標」：`net_edge_pct ≥ net_edge_threshold_pct`；
兩腿的 `data_status` 皆為 `LISTED` 且各自帶有有效的 funding 週期（以 struct 欄位或反序列化繞過建構子產生的「LISTED 但無週期」觀測 SHALL 視為不達標）；兩腿的 `volume_24h_quote` 皆不小於 `min_24h_volume_usdt`；
兩個交易所皆在 `allowed_exchanges`；若 `allowed_coins` 非空，該標的在其中。
缺少成交量資料 SHALL 視為 0（失敗即封閉），不得視為無限大。

#### Scenario: 任一腿成交量不足即不達標

- **WHEN** 一腿成交量為 `min_24h_volume_usdt` 以上、另一腿低於，且 Net Edge 達門檻
- **THEN** 不達標

#### Scenario: 成交量缺失視為不足

- **WHEN** 某腿沒有成交量資料
- **THEN** 不達標

#### Scenario: Gross Spread 不影響達標

- **WHEN** gross spread 很大但 `net_edge_pct` 低於門檻
- **THEN** 不達標

### Requirement: 多空方向取 Net Edge 較高者，不由 rate 高低固定

對同一標的在兩個交易所的觀測，系統 SHALL 兩個方向都計算 Net Edge，並回傳較高者；兩者相同時保留第一個參數為 long。
方向 SHALL NOT 單純由 rate 高低決定：單次結算模型下只有在 `T` 結算的腿會收付，因此較划算的方向同時取決於各腿的結算時間與 rate。
系統 SHALL 同時回傳 gross spread（兩 rate 差的絕對值，僅供顯示）。

覆蓋三所時，SHALL 對所有可配對的組合計算：**達標者優先於不達標者，其次取 Net Edge 最高者**；沒有任何組合達標時，回傳 Net Edge 最高者並標示不達標。
各配對 SHALL 使用該配對生效的參數（涉及的兩個交易所的覆寫合併後的值），而不是所有配對共用同一組。

#### Scenario: 兩個 rate 皆為正且週期不同時方向由結算腿決定

- **WHEN** a 為 +0.0002（下次結算 4 小時後）、b 為 +0.0005（下次結算 8 小時後），成本皆為 0，`N = 1000`
- **THEN** 只有 a 會在 `T` 結算；a 作 long 會付 0.2 USDT、a 作 short 會收 0.2 USDT，因此回傳 a 為 short、b 為 long，Net Edge 為 0.2 USDT，且參數交換順序結果相同

#### Scenario: 兩腿同時結算時 rate 較低者為 long

- **WHEN** a 為 +0.0003、b 為 −0.0001，兩腿皆在同一時間結算，成本為 0，`N = 1000`
- **THEN** b 為 long、a 為 short，Net Edge 為 0.4 USDT

#### Scenario: 三所取 Net Edge 最高的配對

- **WHEN** 同一標的有 Binance、Bybit、OKX 三筆觀測
- **THEN** 對三種配對各自計算，回傳達標者中 Net Edge 最高者及其 long / short

#### Scenario: 達標者優先於較高但不達標者

- **WHEN** OKX 配對的 Net Edge 最高但缺少成交量資料而不達標，Binance 與 Bybit 配對較低但達標
- **THEN** 回傳 Binance 與 Bybit 配對，且標示達標

#### Scenario: 沒有任何組合達標

- **WHEN** 所有組合都不達標
- **THEN** 回傳 Net Edge 最高的組合並標示不達標

#### Scenario: 每個配對套用自己的參數

- **WHEN** 涉及 OKX 的配對有較高的 `est_slippage_pct` 覆寫，其餘配對沒有
- **THEN** 涉及 OKX 的配對以較高滑價計算 Net Edge，選擇結果可能因此不同於共用單一參數時

### Requirement: 手續費與滑價為必填設定

`taker_fee_pct`（每個交易所各一）與 `est_slippage_pct`、`safety_margin_pct` SHALL 由風控設定提供，
core SHALL NOT 內建任何預設費率；設定缺失時 Net Edge 計算 SHALL 回傳錯誤，而不是以 0 代替。

#### Scenario: 缺少費率時回傳錯誤

- **WHEN** 計算 Net Edge 時某交易所沒有 `taker_fee_pct`
- **THEN** 回傳「缺少費率設定」錯誤，不產生 Net Edge 數值

