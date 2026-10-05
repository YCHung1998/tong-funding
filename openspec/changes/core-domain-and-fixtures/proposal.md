## Why

Backend（`mvp-python`）最有價值的資產是**純邏輯與它的測試**，不是程式碼本身。
這個 change 把那些純邏輯搬進 Rust 的 `core` crate，同時修掉抗辯中確認的設計缺陷：
funding 週期被忽略、Net Edge 單位不一致、滑價一個欄位承擔兩種相反方向的語意、
`PARTIAL_FAILURE` 沒有被型別禁止自動轉出。
`core` 不含任何 I/O，是後續 store / exchange / engine 共同依賴的地基。

## What Changes

- 新增 `core` crate 的領域型別：Decimal 數值、`FundingObservation`（含 funding 週期與雙時間戳）、`Quantity`、Pair 狀態 enum。
- 新增 Net Edge 計算（**單次結算模型**）與達標判定。這是 Python 版沒有的新邏輯，不是搬移。
- 搬移並修正：下單數量取整（`quantity_precision`）、送單前檢查（`pretrade_check`，新增資料過期檢查）、持倉分組（`position_grouping`）、風控設定（`risk_config`，欄位重新命名與精簡）。
- 新增「每腿保守值」的風控覆寫合併規則，並要求它是可被執行路徑直接呼叫的純函式（Python 版只存不讀）。
- 新增 parity fixtures：由 Python 純函式匯出 JSON，Rust 逐筆比對；僅限純函式，不含整合行為。
- **BREAKING（相對 Python 版）**：`max_slippage_pct` 拆成 `max_price_drift_pct` 與 `est_slippage_pct`；移除 `hedge_threshold_pct`、`funding_threshold_pct`、`max_concurrent_trades`；`execution_mode` 值改為 `SIMULATION` / `EXCHANGE_DEMO`。

## Capabilities

### New Capabilities
- `funding-observation`: 標準化的 funding 觀測資料、funding 週期推導、資料狀態與過期判定、結算時間。
- `net-edge`: 單次結算模型下的 Net Edge 計算與達標判定；8h 等效僅供顯示。
- `quantity-precision`: 依交易所 lot size 向下取整、OKX 張數換算、由交易所持倉建構數量。
- `pretrade-validation`: 送單前以最新資料重新驗證的具名檢查清單。
- `pair-lifecycle`: Pair 狀態 enum、合法轉移表、禁止自動轉出的狀態。
- `position-grouping`: 把已對沖的兩腿持倉配成一組。
- `risk-config`: 風控欄位定義、預設值、驗證、每腿保守值的合併規則。
- `parity-fixtures`: 由 Python 純函式匯出的 golden fixtures 及其比對方式。

### Modified Capabilities
<!-- 無 -->

## Impact

- 新增 `core/` crate 內容（依賴：`rust_decimal`、`serde`、`serde_json`；不得依賴 GPUI 或任何網路 crate）。
- 新增 `tools/dump_fixtures.py`（唯讀匯入 `mvp-python` 的純函式，不修改該專案）。
- 新增 `core/tests/fixtures/*.json`。
- 依賴 change `bootstrap-gpui-shell` 已建立的 workspace。
