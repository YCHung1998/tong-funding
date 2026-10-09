## 0. 前置

- [x] 0.1 確認 `okx-signed-read`、`okx-demo-execution`、`okx-funding-ledger`、`trade-cost-estimate` 已合併；執行 `openspec validate --strict` 全部通過後才開始

## 1. 可下單集合與掃幣（先寫失敗測試）

- [x] 1.1 `bridge.rs`：`TRADABLE_EXCHANGES`、`ACCOUNT_EXCHANGES` 改為三所；`scanner.rs` 移除 `compare_only`，方向與達標在 `allowed_exchanges` 內三所計算；更新 `scanner_tests.rs`、`scan_view_tests.rs`、`scan_table_ui_tests.rs`，涵蓋 spec 的方向與排除情境
- [x] 1.2 `candidates.rs`：移除 `CandidateBlock::CompareOnly`；含 OKX 的列依一般規則；更新 `candidates_tests.rs`

## 2. 下單相關頁面

- [x] 2.1 `manual_order.rs`：OKX 面板、幣量輸入以 `Quantity::okx_contracts` 換張數、顯示「N 張（≈ x 幣，≈ y USDT）」、`ctVal` 未知停用、持倉選擇器帶入幣量；更新 `manual_order_tests.rs`
- [x] 2.2 `staged_orders.rs`：OKX 保證金列；`trade-cost-estimate` 的「會吃到第二檔」對 OKX 改以張數 × `ctVal` 比較、`ctVal` 未知顯示「無法判斷」；更新 `staged_orders_tests.rs`

## 3. 帳戶頁面

- [x] 3.1 `ui/live.rs`：OKX 帳戶輪詢結果轉為 `AccountState`（資產 `ccy/eq/eqUsd`、合約權益 = Σ 持倉 `imr` 與可用保證金、帳戶模式不支援的原因）；純函式與測試
- [x] 3.2 `dashboard.rs` / `pages.rs`：OKX 真實帳戶卡，移除 `CompareOnly` 與 `OKX_NOTE`；更新 `dashboard_tests.rs`
- [x] 3.3 `positions.rs`：OKX 持倉列（幣量 + 張數、`ctVal` 未知標示）、移除 OKX 註記、分組以幣量；更新 `positions_tests.rs`

- [x] 3.4 （自 `okx-signed-read` 3.2 移入）`OkxPosition` → `models::Position` 頁面用轉換純函式與測試（幣量 = 張數 × `ctVal`，`ctVal` 未知標示無法換算）
- [x] 3.5 （自 `okx-signed-read` 3.3 移入）`ui/live.rs`：建立 `OkxSignedClient`（OKX 校時偏移、`ClockResync` 走公開時間端點）並以 `DemoAccountView::with_okx` 接上，加入帳戶輪詢與 `LegAccount` 輪詢迴圈；`start_funding_loop` 傳入 `OkxLedgerSource`（`okx-funding-ledger` 的 2.4 移入，OKX 金鑰以 `load_credentials(.., Okx, true)` 判斷就緒）

- [x] 3.6 （自 `okx-execution-guards` 移入）接線：`DemoExecutorFactory::with_okx_limits`（公開 instruments 的 `ctVal` / `lotSz`、標記價、風險設定的單腿名目上限）、`OkxSignedClient::with_latch(factory.okx_latch())`（工廠擁有唯一的 `Arc<OkxLatch>`，下單客戶端已共用）、`OkxLatch::reason()` 顯示為畫面橫幅；未接 limits 前 OKX 單全部 `not_sent`

- [x] 3.7 （自 `okx-execution-guards` 第三輪移入）①手動 OKX 下單（含 `reduceOnly`）必須在進入 engine 前把幣量換成張數（`Quantity::okx_contracts`），並為開倉填 `intended_base_qty`；②`engine/recovery.rs` 的 `okx_ct_val.unwrap_or(Decimal::ONE)` 對 OKX 改為「缺 ctVal = 錯誤」；③平倉 `lotSz` 檢查：持倉期間 `lotSz` 可能被交易所調大，使既有持倉張數不再是整數倍——本計畫僅文件化，不放寬檢查（放寬需持倉資料）；④平倉被拒是因為「沒有持倉」時，「對側腿裸露」警示文字不適用，需區分（例如 `51169` 類無持倉碼）

- [x] 3.8 OKX leverage sync（`order-leverage-sync` 合併後新增）：實作 OKX `set-leverage`（`POST /api/v5/account/set-leverage`，`tdMode=cross`，經 `OkxHost::target()`，帶模擬標頭）與每標的最大槓桿查詢（`AccountView::max_leverage` 對 OKX），讓 engine 開倉腿帶的 `leverage` 能在送單前套用；完成前 executor 對帶 `leverage` 的 OKX 開倉一律 `not_sent`（「OKX leverage sync not implemented yet」，送出任何請求之前）。本 change 的 task 數已偏多（>12），建議把 3.4–3.8 拆為第二個 change `okx-trading-enablement-2`

## 4. 驗證

- [x] 4.1 `cargo test -p tong-funding`、`cargo test -p tong-funding-core` 全套綠燈；`cargo clippy --all-targets -- -D warnings`；以 `rg -n "僅比價|CompareOnly|OKX_NOTE" app/src` 確認無殘留
- [ ] 4.2 實機（使用者執行；agent 不得讀 Keychain、不得送單）：啟動 app（EXCHANGE_DEMO），確認：總覽 OKX 卡數字與 OKX demo 網頁一致；掃幣頁出現含 OKX 的方向；在手動下單頁對 OKX 以小額幣量開倉後平倉，持倉頁顯示幣量與張數正確；交易單頁送出一組含 OKX 的配對並於平倉後檢查 PnL 不再是「OKX 價格未知」

實作備註：3.1 的 OKX 轉換在 `ui/live.rs`（`okx_to_position` / `okx_account_data`）；3.7 ③ 僅文件化，④ 因拒絕白名單不得變更，僅調整 51169 的「不明」原因文字；單腿名目上限取合約範本 notional × 2；`max_leverage` 為標的上限（大名目分級未驗證，set-leverage 為最終檢查）。4.2 實機項目未勾，由使用者執行：`cargo run`（EXCHANGE_DEMO，OKX demo 金鑰已在 Keychain），並依序驗證總覽 OKX 卡、掃幣方向、手動 OKX 小額開倉（含槓桿）與平倉、交易單含 OKX 配對。

## 5. 抗辯修正（enablement）

- [x] 5.1 F1 / F2：`Executor::prepare_open` 送前屏障、`OrderRequest.notional_usdt`、引擎在 `OpenPrepared` 後才送兩腿；流程與 executor 測試
- [x] 5.2 F3 set-leverage 回覆驗證；F4 ctVal 缺失為錯誤
- [x] 5.3 F5 OKX 警示與 demo 金鑰依 `allowed_exchanges`；F6 掃幣頁每列判斷測試；F7 儀表板固定值對齊
- [x] 5.4 簡化項目（見 design）
- [x] 5.5 第二輪 G1–G7：`prepared` 標記、送前窗口（kill switch / 視窗 / 逾時 / 手動單）、instId 必填、demo 金鑰即時重算；骨架回歸與真實 executor 流程測試
- [x] 5.6 第三輪 H1–H6：兩腿意圖原子落地、裸腿文字涵蓋所有非成功、校時偏移、手動單 / 重複配對進場衝突、屏障期限、簡化
