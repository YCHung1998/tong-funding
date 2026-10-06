## 1. 請求建構（先寫失敗測試）

- [ ] 1.1 `execution/http.rs`：`DemoEnv::Okx(OkxHost)`，`to_demo` 只經 `OkxHost::target()` 取網址並套用模擬標頭；`ReqwestOrderTransport` 缺標頭零連線；測試同 `okx-signed-read` 1.2 的方式
- [ ] 1.2 `execution/endpoints.rs` 新增 OKX 下單 / 查單 / 撤單路徑常數；`execution/okx.rs` 的 `submit_request` / `query_request` / `cancel_request`（`instId` 轉換純函式與 `public/okx.rs` 相同測試向量、`clOrdId` 英數 ≤ 32 檢查、不帶 `posSide`、POST 本文即簽名本文）；測試涵蓋 spec 參數情境
- [ ] 1.3 `static_checks.rs`：`execution_names_no_host_literal_and_has_no_okx_request` 演進為「允許 OKX 下單客戶端，禁止主機與 `x-simulated-trading` 字面」，附反例測試

## 2. 回應判讀（錄製回應測試）

- [ ] 2.1 fixtures `app/tests/fixtures/okx/orders/`：`place_accepted.json`、`place_rejected_51131.json`、`place_code0_scode51121.json`、`place_50004.json`、`place_50011.json`、`order_{live,filled,canceled}.json`、`order_51603.json`、`cancel_{accepted,51400}.json`（依文件構造、`.meta` 註明未驗證）
- [ ] 2.2 `classify.rs`：`okx_reply`（`code` + `sCode` 雙層）、OKX 未知 / 限流 / 查無代碼表、`okx_state`；測試涵蓋 spec 的分類情境
- [ ] 2.3 `execution/okx.rs`：`submit`、`query`（`accFillSz` 張數、`avgPx`、手續費取負號）、`cancel`（撤後回讀、`51400` 回讀）、`account_mode`（沿用 `okx-signed-read` 的讀數與 60 秒快取）；`50102` 於查單 / 撤單重新校時重試一次、送單不重試

## 3. 執行器與工廠

- [ ] 3.1 `executor.rs`：`DemoExecutor` 加入 `Option<OkxOrderClient>`，移除 `OKX_UNSUPPORTED` 分支；`ensure_one_way` 的 OKX 鍵為帳戶層（`"*"`）；缺客戶端 → `not_sent`（原因含缺少項目）
- [ ] 3.2 `factory.rs`：OKX `load_credentials(.., true)` 可選；`ui/live.rs` 的 `demo_keys` 與工廠呼叫配合；測試：缺 OKX passphrase 仍可建執行器且 OKX 單 `not_sent`
- [ ] 3.3 `executor_tests.rs` / `replay_tests.rs`：介面契約測試加入 OKX 參數化；記錄每個請求並斷言主機、模擬標頭、無重送

## 4. 驗證

- [ ] 4.1 `cargo test -p tong-funding`、`cargo test -p tong-funding-core` 全套綠燈；`cargo clippy --all-targets -- -D warnings`
- [ ] 4.2 實機（使用者執行；agent 不得讀 Keychain、不得送單）：`live_probe.rs` 加入 `TONG_DEMO_EXCHANGES=binance,okx`（或 `bybit,okx`）與 OKX 校時；使用者執行 `TONG_DEMO_LIVE=<確認字串> TONG_DEMO_EXCHANGES=bybit,okx TONG_DEMO_SYMBOL=ETHUSDT TONG_DEMO_QTY=<幣量> cargo test -p tong-funding live_demo_probe -- --ignored --nocapture`，回報：ACK 後第一次查單的 `state`、成交張數 × `ctVal` 是否等於另一腿幣量、手續費與 OKX demo 網頁一致、平倉後兩腿持倉為 0；以去識別化的真實回應替換 2.1 fixtures
