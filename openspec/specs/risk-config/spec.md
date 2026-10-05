# risk-config Specification

## Purpose
TBD - created by archiving change core-domain-and-fixtures. Update Purpose after archive.
## Requirements
### Requirement: 風控欄位、預設值與驗證

全域風控設定 SHALL 包含下列欄位，預設值來自 Python 版（`risk_config.py`）並標明例外：

| 欄位 | 預設 | 驗證 |
|---|---|---|
| `max_leverage` | 5 | > 0 |
| `max_concurrent_pairs` | 3 | 整數 ≥ 1 |
| `max_price_drift_pct` | 0.05 | > 0（Python 版名為 `max_slippage_pct`） |
| `stale_data_threshold_ms` | 1000（使用者 2026-10-05 拍板，與 Python 版相同；Figma 的 5 秒需修正） | > 0 |
| `safety_margin_pct` | 0.01（使用者 2026-10-05 拍板；百分比數值，代表 0.01%） | ≥ 0 |
| `order_timeout_seconds` | 15 | 整數 ≥ 1 |
| `max_leg_imbalance_pct` | 1.0 | ≥ 0 |
| `min_24h_volume_usdt` | 50000 | ≥ 0 |
| `allowed_exchanges` | Binance、Bybit、OKX | 至少一個 |
| `allowed_coins` | 空（代表不限制） | — |
| `execution_mode` | `SIMULATION` | `SIMULATION` 或 `EXCHANGE_DEMO` |
| `trigger_mode` | `AUTO` | `AUTO` 或 `MANUAL` |

所有以 `_pct` 結尾的欄位 SHALL 為百分比數值（0.01 代表 0.01%）。
另有下列欄位 SHALL 沒有內建預設值、須由使用者填寫：`net_edge_threshold_pct`、`est_slippage_pct`，以及每個交易所各一的 `taker_fee_pct`。
任一必填欄位缺失時，設定 SHALL 被標為「不完整」，Net Edge 與達標判定 SHALL 因此不可用，而不是以 0 代替。

下列 Python 版欄位 SHALL NOT 存在：`hedge_threshold_pct`（與 `max_leg_imbalance_pct` 重複）、`funding_threshold_pct`（達標改由 Net Edge 決定）、`max_concurrent_trades`（與 `max_concurrent_pairs` 重複）。

#### Scenario: 首次啟動的預設是安全的

- **WHEN** 載入一份完全空白的設定
- **THEN** `execution_mode` 為 `SIMULATION`，且設定被標為「不完整」，因為費率與門檻尚未填寫

#### Scenario: 非法值被拒絕

- **WHEN** 嘗試把 `max_leverage` 設為 0 或 `execution_mode` 設為 `LIVE`
- **THEN** 驗證失敗並回傳具體欄位名稱，設定不被改動

### Requirement: 載入設定時必須驗證

從儲存的資料載入風控設定 SHALL 經過驗證：僅以通用的反序列化會接受的非法值（例如 `max_leverage = 0`、`max_concurrent_pairs = 0`、空的 `allowed_exchanges`）SHALL 在載入時被拒絕並指出欄位名稱，不得得到一個看似有效的設定物件。

#### Scenario: 載入非法設定

- **WHEN** 載入內容為 `max_leverage = 0`、`max_concurrent_pairs = 0`、`allowed_exchanges = []` 的設定
- **THEN** 載入失敗並指出具體欄位名稱

#### Scenario: 載入合法設定

- **WHEN** 載入一份由預設值序列化而來的設定
- **THEN** 得到與預設值相同的設定

### Requirement: 每腿風控覆寫與保守值合併

系統 SHALL 允許針對個別交易所覆寫下列欄位：`max_leverage`、`max_price_drift_pct`、`stale_data_threshold_ms`、`order_timeout_seconds`、`max_leg_imbalance_pct`、`min_24h_volume_usdt`、`net_edge_threshold_pct`、`est_slippage_pct`、`safety_margin_pct`。
`max_concurrent_pairs`、`allowed_exchanges`、`allowed_coins`、`execution_mode`、`trigger_mode` SHALL 只存在於全域，不可覆寫。
`taker_fee_pct` 本身就是每個交易所各一個值，不屬於覆寫機制。

系統 SHALL 提供純函式，輸入全域設定、各交易所覆寫、配對的 long 與 short 交易所，輸出「這個配對生效的設定」。
每個欄位 SHALL 先套用各腿所在交易所的覆寫（沒有覆寫則用全域值），再取較保守者：

| 取較小者 | 取較大者 |
|---|---|
| `max_leverage`、`max_price_drift_pct`、`stale_data_threshold_ms`、`order_timeout_seconds`、`max_leg_imbalance_pct` | `min_24h_volume_usdt`、`net_edge_threshold_pct`、`est_slippage_pct`、`safety_margin_pct` |

#### Scenario: 槓桿取較小

- **WHEN** 全域 `max_leverage` 為 5，Bybit 覆寫為 4，配對為 long Binance、short Bybit
- **THEN** 該配對生效的 `max_leverage` 為 4

#### Scenario: 估計滑價取較大

- **WHEN** 全域 `est_slippage_pct` 為 0.01，Binance 覆寫為 0.03，配對涉及 Binance 與 Bybit
- **THEN** 該配對生效的 `est_slippage_pct` 為 0.03

#### Scenario: 兩腿皆無覆寫

- **WHEN** 兩個交易所都沒有覆寫
- **THEN** 生效值等於全域值

#### Scenario: 不可覆寫的欄位被拒絕

- **WHEN** 嘗試對 Bybit 覆寫 `execution_mode`
- **THEN** 驗證失敗，指出該欄位只能存在於全域

