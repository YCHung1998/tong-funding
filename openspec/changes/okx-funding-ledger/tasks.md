## 1. 流水解析與客戶端（先寫失敗測試）

- [x] 1.1 fixtures `app/tests/fixtures/funding/`：`okx_bills_funding_page1_full.json`（100 列，含非資金費列）、`okx_bills_funding_page2_short.json`、`okx_bills_sign_mismatch.json`、`okx_bills_repeat_cursor.json`（依文件構造、`.meta` 註明未驗證）
- [x] 1.2 `signed/ledger.rs` `parse_okx_bills`（另有 `core` 的 `FillRecord.contract_value_missing` / `IncompleteReason::MissingContractValue`，見 design「實作時發現」）：只取 173/174、`balChg` 帶號、`subType` 正負號檢查、`ccy` 必須 USDT、`instId` → `BASEUSDT`、`rows` 為原始筆數；測試涵蓋 spec 的解析情境
- [x] 1.3 `OkxLedgerClient::bills_page(symbol, start, end, after)`：經 `okx-signed-read` 的請求建構（模擬標頭、簽名、`50102` 重試一次、`50011` 限流）；錄製測試斷言 query 參數與 `after` 傳遞

## 2. 抓取與 PnL

- [x] 2.1 `funding/fetch.rs`：OKX `LedgerSource`（`per_symbol = true`、`next_cursor` = 最後一列 `billId`、重複游標 / 頁數上限 → 失敗）；移除 `plan_fetches` 對 OKX 的跳過；測試涵蓋 spec 分頁情境
- [x] 2.2 `engine/actor.rs`：OKX 腿的 `ORDER_SUBMITTED` / `ORDER_FILL` payload 加 `ct_val`；engine 測試斷言 OKX 有、Binance / Bybit 無
- [x] 2.3 `funding/pnl_record.rs`：OKX 腿數量 = `filled_quantity × ct_val`、使用 `avg_price` 與參考價、缺 `ct_val` → INCOMPLETE；OKX funding 依抓取狀態；更新檔頭註解；測試涵蓋 spec 情境並鎖定 Binance / Bybit PnL 不變
- [x] 2.4 `funding/runner.rs`、`funding/reconcile.rs`：OKX 來源納入抓取與對帳、金鑰不可用略過（`runner` / `reconcile` 對任何有來源的腿一視同仁，測試涵蓋 OKX 抓取、對帳、金鑰缺少略過）；`ui/live.rs` 的 `start_funding_loop` 傳入 OKX 來源移至 `okx-trading-enablement` 3.5（與 `OkxSignedClient` 的建立一起，見其 task）

## 3. 驗證

- [x] 3.1 `cargo test -p tong-funding`、`cargo test -p tong-funding-core` 全套綠燈；`cargo clippy --all-targets -- -D warnings`
- [ ] 3.2 實機（使用者執行；agent 不得讀 Keychain、不得送單）：以 `okx-demo-execution` 4.2 的探針（或手動下單頁，待 `okx-trading-enablement`）開一個跨越 OKX 結算時間的小額 demo 配對，結算 2 分鐘後執行 `cargo test -p tong-funding okx_live_bills_probe -- --ignored --nocapture`（本 change 新增，只發 GET），回報：帳單是否出現、`subType`、`balChg` 與 OKX demo 網頁一致、結算到出現的延遲；平倉後確認持倉頁 / PnL 顯示 OKX 腿 funding 與價格分量；以去識別化真實回應替換 1.1 fixtures
