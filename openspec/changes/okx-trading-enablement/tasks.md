## 0. 前置

- [ ] 0.1 確認 `okx-signed-read`、`okx-demo-execution`、`okx-funding-ledger`、`trade-cost-estimate` 已合併；執行 `openspec validate --strict` 全部通過後才開始

## 1. 可下單集合與掃幣（先寫失敗測試）

- [ ] 1.1 `bridge.rs`：`TRADABLE_EXCHANGES`、`ACCOUNT_EXCHANGES` 改為三所；`scanner.rs` 移除 `compare_only`，方向與達標在 `allowed_exchanges` 內三所計算；更新 `scanner_tests.rs`、`scan_view_tests.rs`、`scan_table_ui_tests.rs`，涵蓋 spec 的方向與排除情境
- [ ] 1.2 `candidates.rs`：移除 `CandidateBlock::CompareOnly`；含 OKX 的列依一般規則；更新 `candidates_tests.rs`

## 2. 下單相關頁面

- [ ] 2.1 `manual_order.rs`：OKX 面板、幣量輸入以 `Quantity::okx_contracts` 換張數、顯示「N 張（≈ x 幣，≈ y USDT）」、`ctVal` 未知停用、持倉選擇器帶入幣量；更新 `manual_order_tests.rs`
- [ ] 2.2 `staged_orders.rs`：OKX 保證金列；`trade-cost-estimate` 的「會吃到第二檔」對 OKX 改以張數 × `ctVal` 比較、`ctVal` 未知顯示「無法判斷」；更新 `staged_orders_tests.rs`

## 3. 帳戶頁面

- [ ] 3.1 `ui/live.rs`：OKX 帳戶輪詢結果轉為 `AccountState`（資產 `ccy/eq/eqUsd`、合約權益 = Σ 持倉 `imr` 與可用保證金、帳戶模式不支援的原因）；純函式與測試
- [ ] 3.2 `dashboard.rs` / `pages.rs`：OKX 真實帳戶卡，移除 `CompareOnly` 與 `OKX_NOTE`；更新 `dashboard_tests.rs`
- [ ] 3.3 `positions.rs`：OKX 持倉列（幣量 + 張數、`ctVal` 未知標示）、移除 OKX 註記、分組以幣量；更新 `positions_tests.rs`

- [ ] 3.4 （自 `okx-signed-read` 3.2 移入）`OkxPosition` → `models::Position` 頁面用轉換純函式與測試（幣量 = 張數 × `ctVal`，`ctVal` 未知標示無法換算）
- [ ] 3.5 （自 `okx-signed-read` 3.3 移入）`ui/live.rs`：建立 `OkxSignedClient`（OKX 校時偏移、`ClockResync` 走公開時間端點）並以 `DemoAccountView::with_okx` 接上，加入帳戶輪詢與 `LegAccount` 輪詢迴圈

- [ ] 3.6 （自 `okx-execution-guards` 移入）接線：`DemoExecutorFactory::with_okx_limits`（公開 instruments 的 `ctVal` / `lotSz`、標記價、風險設定的單腿名目上限）、`OkxLatch` 由 `OkxSignedClient` 與下單客戶端共用、`OkxLatch::reason()` 顯示為畫面橫幅；未接 limits 前 OKX 單全部 `not_sent`

## 4. 驗證

- [ ] 4.1 `cargo test -p tong-funding`、`cargo test -p tong-funding-core` 全套綠燈；`cargo clippy --all-targets -- -D warnings`；以 `rg -n "僅比價|CompareOnly|OKX_NOTE" app/src` 確認無殘留
- [ ] 4.2 實機（使用者執行；agent 不得讀 Keychain、不得送單）：啟動 app（EXCHANGE_DEMO），確認：總覽 OKX 卡數字與 OKX demo 網頁一致；掃幣頁出現含 OKX 的方向；在手動下單頁對 OKX 以小額幣量開倉後平倉，持倉頁顯示幣量與張數正確；交易單頁送出一組含 OKX 的配對並於平倉後檢查 PnL 不再是「OKX 價格未知」
