## Why

唯讀頁面完成、引擎可在 demo 下單後，剩下需要使用者操作的四個頁面：選擇要交易的配對、設定交易規模與風控、以及除錯用的手動下單。
這四頁是使用者唯一能「讓系統送出訂單」或「改變送單規則」的入口，因此要求特別嚴：送出前逐腿確認、設定不完整就禁止、單腿失敗只由人工處理、手動下單不得繞過 `execution_mode`。

## What Changes

- **交易單**：已暫存配對清單、勾選與全選、每筆的雙腿數量／名目／保證金／槓桿與 Net Edge、`trigger_mode`（AUTO / MANUAL）切換、一鍵送出已選取（先列出將送出的每一腿並二次確認）、上次執行結果（涵蓋 `SIMULATION` 與 `EXCHANGE_DEMO`，如實標示該次模式）、單腿失敗／不平衡的人工處理入口（人工要求平倉、人工確認已平倉）。
- **合約設定**：每腿目標名目本金、槓桿與保證金雙向連動、依即時價格的數量試算（顯示依 lot size 向下取整後的數量與「低於最小下單量」提示；OKX 以合約張數顯示）。
- **風控設定**：全域欄位（依 `risk-config`；相對 Figma 移除 Funding Threshold、Max Concurrent Trades、Hedge Threshold）、Order Timeout 改秒、Max Slippage 拆成「最大價格漂移」與「估計滑價」、Net Edge 必填欄位（門檻、估計滑價、安全邊際、各所 taker 費率）、`stale_data_threshold_ms` 預設 1000、各交易所覆寫（接進執行路徑，雙腿取保守值）、模式單選 `SIMULATION` / `EXCHANGE_DEMO`（無 LIVE）。
- **手動下單**：除錯用單腿下單與撤單；**依 `execution_mode` 走 engine 的單一下單路徑**（`SIMULATION` 下送往 `SimulatedExecutor` 並標示模擬，`EXCHANGE_DEMO` 下送 demo 帳戶；依已合併的 `engine-simulation` spec），與標準流程以分隔線和警示隔開。
- **掃幣「加入交易單」與 Candidate List**（spec `scanner-candidates`）。
- **`min_expected_net_pnl_pct`** 與 `net_edge_threshold_pct` 並存（MODIFIED `risk-config` / `pretrade-validation`）。
- **組裝根**：app 啟動時以單一 `Db` 建立真實 `EngineDeps` 並啟動 engine，頁面讀 engine snapshot、送 Command。
- 設定不完整（缺費率或門檻）時，相關頁面明確顯示原因，且不允許送出。
- 與 Figma 的刻意差異與 Python 版行為差異，集中記錄在 `design.md`。

## Capabilities

### New Capabilities
- `staged-orders-page`: 交易單頁（清單、選取、二次確認、禁用條件、上次執行結果、人工處理入口、`trigger_mode`）。
- `contract-settings-page`: 合約設定頁（模板、雙向連動、數量試算）。
- `risk-settings-page`: 風控設定頁（欄位與驗證、Net Edge 必填欄位、各所覆寫、模式單選）。
- `manual-order-page`: 手動下單頁（除錯定位、受模式約束、單腿下單與撤單）。

- `scanner-candidates`: 掃幣頁「加入交易單」欄、Candidate List、加入並前往交易單。

### Modified Capabilities
- `risk-config`: 新增全域欄位 `min_expected_net_pnl_pct`（預設 0.03）。
- `pretrade-validation`: `NetEdgeQualified` 另需預期淨收益 ≥ `min_expected_net_pnl_pct`（十項檢查不變）。

## Impact

- 依賴 `engine-simulation` 與 `exchange-demo-execution`；所有送單與撤單動作只能走引擎的單一下單路徑（UI 不持有任何交易所 client）。
- 依賴 `core-domain-and-fixtures`（`risk-config`、`net-edge`、`quantity-precision`、`pretrade-validation`、`pair-lifecycle`）與 `store-sqlite`（設定持久化、事件）。
- 依賴 `bootstrap-gpui-shell`（theme 色票與字型，本 change 不重寫色碼）與 `ui-readonly-pages`（全頁警示橫幅 `alert-banner`；本 change 只負責橫幅旁的人工處理入口）。
- 範圍界線：掃幣、總覽、持倉、系統日誌四頁屬 `ui-readonly-pages`；OKX 在第一版只做公開行情，不下單。
