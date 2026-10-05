## ADDED Requirements

### Requirement: 全域欄位、單位與已移除欄位

風控設定頁 SHALL 以 `risk-config` 為唯一欄位來源，在「Global Limits」與「Layer 1 進階配對規則」下提供下列欄位，且每個欄位 SHALL 標示單位：

| 欄位 | 單位標示 |
|---|---|
| `max_leverage` | × |
| `max_price_drift_pct`（標籤「最大價格漂移」，說明為每腿成交價相對基準價的偏移上限） | % |
| `stale_data_threshold_ms`（預設 1000） | ms |
| `max_concurrent_pairs` | pairs |
| `order_timeout_seconds`（預設 15） | 秒 |
| `max_leg_imbalance_pct` | % |
| `min_24h_volume_usdt` | USDT |
| `min_expected_net_pnl_pct`（標籤「Min Expected Net PnL %」，預設 0.03，只存在於全域） | % |
| `allowed_exchanges`、`allowed_coins` | — |

所有 `_pct` 欄位 SHALL 為百分比數值（0.01 代表 0.01%），單位標示 SHALL 為 `%`，輸入與顯示 SHALL NOT 在小數與百分比之間偷偷換算。
頁面 SHALL NOT 出現下列欄位：Funding Threshold（`funding_threshold_pct`）、Max Concurrent Trades（以 legs 計）、Hedge Threshold（`hedge_threshold_pct`）。
Figma 上的「Min Expected Net PnL %」SHALL 保留為 `min_expected_net_pnl_pct`，與 `net_edge_threshold_pct` 並存（使用者 2026-10-05 晚拍板）；頁面 SHALL 以文字區分兩者：Net Edge 門檻扣除安全邊際，Min Expected Net PnL 是扣除手續費與估計滑價、未扣安全邊際的預期淨收益，兩者皆須達到才算 `NetEdgeQualified`。
`max_concurrent_pairs` 旁 SHALL 顯示目前開啟中的配對數（例如「目前 2 / 3 組」）。

#### Scenario: 已移除欄位不存在

- **WHEN** 開啟風控設定頁
- **THEN** 頁面上找不到 Funding Threshold、Max Concurrent Trades 與 Hedge Threshold

#### Scenario: Order Timeout 以秒顯示

- **WHEN** 使用全新設定開啟頁面
- **THEN** Order Timeout 顯示 15，單位標示為「秒」

#### Scenario: 資料過期門檻預設為 1000 毫秒

- **WHEN** 使用全新設定開啟頁面
- **THEN** Stale Data Threshold 顯示 1000，單位標示為 ms，而不是 Figma 示範的 5 秒

#### Scenario: 兩個門檻並存

- **WHEN** 檢視 Global Limits 與 Net Edge 區塊
- **THEN** 同時存在 `net_edge_threshold_pct` 與 `min_expected_net_pnl_pct`（預設 0.03），後者不出現在各交易所覆寫區塊

#### Scenario: 價格漂移與滑價為兩個欄位

- **WHEN** 檢視 Global Limits
- **THEN** 同時存在「最大價格漂移」與「估計滑價」兩個獨立欄位，且沒有單一的 Max Slippage 欄位

### Requirement: Net Edge 區塊與「設定不完整」狀態

頁面 SHALL 提供 Net Edge 區塊，含下列欄位：`net_edge_threshold_pct`、`est_slippage_pct`、`safety_margin_pct`（預設 0.01），以及每個支援的交易所（Binance、Bybit、OKX）各一個 `taker_fee_pct`。
`net_edge_threshold_pct`、`est_slippage_pct` 與各所 `taker_fee_pct` SHALL 沒有預設值，未填時欄位 SHALL 顯示為空白並標示「必填」，SHALL NOT 以 0 顯示或儲存。
任一必填欄位缺失時，頁面 SHALL 於頂部顯示「設定不完整」，並列出所有缺漏的欄位名稱；此狀態 SHALL 可被交易單頁引用以禁用送出。
區塊底部的預檢摘要 SHALL 以目前設定值呈現 Net Edge 公式（預期 funding 收入 − 4 筆成交的手續費 − 4 筆成交的估計滑價 − 安全邊際 ≥ `net_edge_threshold_pct`），並 SHALL 說明 `est_slippage_pct` 以四筆成交各估一次；缺漏欄位處 SHALL 顯示「未設定」，SHALL NOT 代入 0。

#### Scenario: 全新設定為不完整

- **WHEN** 以完全空白的設定開啟頁面
- **THEN** 頂部顯示「設定不完整」，並列出 `net_edge_threshold_pct`、`est_slippage_pct` 與三所 `taker_fee_pct`

#### Scenario: 補齊後轉為完整

- **WHEN** 使用者填入所有必填欄位並儲存
- **THEN** 「設定不完整」消失，交易單頁不再因設定不完整而禁用送出

#### Scenario: 預檢摘要不代入 0

- **WHEN** Bybit 的 `taker_fee_pct` 尚未填寫
- **THEN** 預檢摘要中 Bybit 的費率位置顯示「未設定」

### Requirement: 驗證、儲存與變更紀錄

每個欄位 SHALL 依 `risk-config` 的驗證規則即時驗證，錯誤訊息 SHALL 帶欄位名稱與規則（例如「max_leverage 必須大於 0」），任一欄位不合法時「儲存風控設定」SHALL 被禁用。
儲存 SHALL 以單一 transaction 持久化全域設定與各所覆寫；失敗時持久化的設定 SHALL 與儲存前完全相同，且頁面 SHALL 顯示失敗原因。
儲存成功 SHALL 寫入不可變的 `RISK_CONFIG_UPDATED` 事件，內容包含變更前與變更後的完整值。
頁面顯示的值 SHALL 以儲存後自資料庫讀回的值為準。

#### Scenario: 非法槓桿

- **WHEN** 使用者把 `max_leverage` 設為 0
- **THEN** 該欄位顯示「max_leverage 必須大於 0」，儲存被禁用，已儲存設定不變

#### Scenario: 儲存留下變更前後值

- **WHEN** 使用者把 `max_leverage` 由 5 改為 4 並儲存
- **THEN** 寫入一筆 `RISK_CONFIG_UPDATED` 事件，內容含 `max_leverage` 前值 5 與後值 4

#### Scenario: 儲存失敗不留半套

- **WHEN** 儲存 transaction 中途失敗
- **THEN** 資料庫中的設定與儲存前相同，頁面顯示失敗原因

### Requirement: 各交易所覆寫必須實際影響送單

頁面 SHALL 為 Binance、Bybit 與 OKX 各提供一個覆寫區塊，每個可覆寫欄位各有「啟用獨立設定」開關，可覆寫的欄位 SHALL 恰為：`max_leverage`、`max_price_drift_pct`、`stale_data_threshold_ms`、`order_timeout_seconds`、`max_leg_imbalance_pct`、`min_24h_volume_usdt`、`net_edge_threshold_pct`、`est_slippage_pct`、`safety_margin_pct`。
`max_concurrent_pairs`、`allowed_exchanges`、`allowed_coins`、`execution_mode`、`trigger_mode` SHALL NOT 出現在覆寫區塊。
開關關閉時欄位 SHALL 顯示繼承的全域值且唯讀；開啟時 SHALL 以目前全域值作為初始值；關閉開關 SHALL 移除該欄位的覆寫。
頁面 SHALL 顯示各交易所配對（Binance×Bybit、Binance×OKX、Bybit×OKX）的生效值預覽，且該預覽 SHALL 由與送單前檢查相同的 `effective_for_pair` 純函式計算（取小：`max_leverage`、`max_price_drift_pct`、`stale_data_threshold_ms`、`order_timeout_seconds`、`max_leg_imbalance_pct`；取大：`min_24h_volume_usdt`、`net_edge_threshold_pct`、`est_slippage_pct`、`safety_margin_pct`）。
儲存的覆寫 SHALL 由執行路徑實際讀取，使覆寫改變送單前檢查的結果；僅儲存而不被讀取 SHALL 視為缺陷。

#### Scenario: 覆寫影響送單前檢查

- **WHEN** 全域 `max_leverage` 為 5，Bybit 覆寫為 4，配對為 long Binance、short Bybit 且槓桿為 5
- **THEN** 該配對的送單前檢查 `Leverage` 失敗，而槓桿同為 5 的 Binance×OKX 配對該項通過

#### Scenario: 生效值預覽取保守值

- **WHEN** 全域 `est_slippage_pct` 為 0.01，Binance 覆寫為 0.03
- **THEN** Binance×Bybit 配對的生效 `est_slippage_pct` 預覽為 0.03

#### Scenario: 不可覆寫的欄位不提供

- **WHEN** 檢視任一交易所的覆寫區塊
- **THEN** 其中沒有 `execution_mode`、`trigger_mode`、`max_concurrent_pairs`、`allowed_exchanges`、`allowed_coins`

#### Scenario: 關閉開關移除覆寫

- **WHEN** 使用者關閉 Bybit `max_leverage` 的獨立設定開關並儲存
- **THEN** Bybit 不再有 `max_leverage` 覆寫，該欄位顯示繼承的全域值

### Requirement: 執行模式單選只有 SIMULATION 與 EXCHANGE_DEMO

頁面 SHALL 以單選提供 `SIMULATION` 與 `EXCHANGE_DEMO`，SHALL NOT 出現 `LIVE` 字樣或選項。
頁面 SHALL 說明：`SIMULATION` 跑完整流程與送單前檢查，訂單由模擬器成交、不送到任何交易所；`EXCHANGE_DEMO` 對 demo / testnet 帳戶真實下單、會真的改變帳戶內的倉位；兩者皆不涉及真錢。
由 `SIMULATION` 切到 `EXCHANGE_DEMO` SHALL 先顯示確認視窗；設定不完整或 demo 金鑰不可用時，`EXCHANGE_DEMO` 選項 SHALL 被禁用並說明原因。
切換 SHALL 寫入事件（含前後值）並即時更新狀態列的模式徽章；切換 SHALL NOT 改變既有倉位，也 SHALL NOT 改變 `trigger_mode`。

#### Scenario: 沒有 LIVE 選項

- **WHEN** 檢視執行模式區塊
- **THEN** 只有 `SIMULATION` 與 `EXCHANGE_DEMO` 兩個選項，且沒有「LIVE 不可執行」警告

#### Scenario: 切換需確認

- **WHEN** 使用者選擇 `EXCHANGE_DEMO`
- **THEN** 先顯示確認視窗，確認前 `execution_mode` 維持 `SIMULATION`

#### Scenario: 設定不完整時禁止切換

- **WHEN** 風控設定為「設定不完整」
- **THEN** `EXCHANGE_DEMO` 選項被禁用，原因指向缺漏的欄位

#### Scenario: 金鑰不可用時禁止切換

- **WHEN** 無法從 Keychain 取得 demo 金鑰
- **THEN** `EXCHANGE_DEMO` 選項被禁用，原因顯示金鑰不可用

#### Scenario: 切換即時反映在狀態列

- **WHEN** 使用者確認切到 `EXCHANGE_DEMO`
- **THEN** 狀態列徽章即時顯示 `EXCHANGE_DEMO`，並寫入一筆含前後值的事件
