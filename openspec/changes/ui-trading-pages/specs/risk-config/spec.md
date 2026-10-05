## MODIFIED Requirements

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
| `min_expected_net_pnl_pct` | 0.03（Python 版 `risk_config.py`；ui-trading-pages 使用者 2026-10-05 晚拍板保留，與 `net_edge_threshold_pct` 並存） | ≥ 0 |

所有以 `_pct` 結尾的欄位 SHALL 為百分比數值（0.01 代表 0.01%）。
另有下列欄位 SHALL 沒有內建預設值、須由使用者填寫：`net_edge_threshold_pct`、`est_slippage_pct`，以及每個交易所各一的 `taker_fee_pct`。
任一必填欄位缺失時，設定 SHALL 被標為「不完整」，Net Edge 與達標判定 SHALL 因此不可用，而不是以 0 代替。

`min_expected_net_pnl_pct` SHALL 只存在於全域（不屬於九個可覆寫欄位，各交易所覆寫中出現它 SHALL 被拒絕並指出欄位名稱），並 SHALL 原樣帶入每個配對的生效設定。
它有預設值，因此 SHALL NOT 使設定變成「不完整」；選擇有預設值而非必填，是為了保持必填欄位清單（與既有測試）不變，且預設值 0.03 比 0（不設限）保守。

下列 Python 版欄位 SHALL NOT 存在：`hedge_threshold_pct`（與 `max_leg_imbalance_pct` 重複）、`funding_threshold_pct`（達標改由 Net Edge 決定）、`max_concurrent_trades`（與 `max_concurrent_pairs` 重複）。

#### Scenario: 首次啟動的預設是安全的

- **WHEN** 載入一份完全空白的設定
- **THEN** `execution_mode` 為 `SIMULATION`，且設定被標為「不完整」，因為費率與門檻尚未填寫

#### Scenario: Min Expected Net PnL 的預設與驗證

- **WHEN** 載入空白設定，再嘗試把 `min_expected_net_pnl_pct` 設為 -0.01
- **THEN** 預設值為 0.03；設為負值時驗證失敗並指出 `min_expected_net_pnl_pct`，設定不被改動

#### Scenario: 非法值被拒絕

- **WHEN** 嘗試把 `max_leverage` 設為 0 或 `execution_mode` 設為 `LIVE`
- **THEN** 驗證失敗並回傳具體欄位名稱，設定不被改動
