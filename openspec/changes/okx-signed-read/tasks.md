## 1. demo 邊界與簽名（先寫失敗測試）

- [ ] 1.1 `signed/endpoints.rs` 新增 `OkxHost::Demo`（`openapi.okx.com`）並加入 `ALLOWED_SIGNED_HOSTS`；`OkxHost::target()` 是取得 OKX 網址的唯一 API，同時回傳 `x-simulated-trading: 1` 常數標頭；OKX GET 建構無條件套用；測試：建構出的每個請求皆帶該標頭、主機等於常數
- [ ] 1.2 `reqwest_transport.rs`：`HostPolicy::SignedDemo` 對 OKX 主機缺標頭或值非 `1` 時零連線拒絕；測試以 `LocalTest` 假伺服器確認沒有連線
- [ ] 1.3 `static_checks.rs`：OKX 主機字面只准在 `signed/endpoints.rs`、`x-simulated-trading` 只准在 `signed/endpoints.rs`、`OkxHost` 沒有其他回傳主機或網址的公開方法、`signed/okx.rs` 無主機字面；各寫一個「違規來源被抓到」的反例測試；更新 `execution_names_no_host_literal_and_has_no_okx_request` 的白名單說明（execution 仍不得有 OKX 請求，留待 `okx-demo-execution`）
- [ ] 1.4 `signing.rs`：`Credentials.passphrase`、`load_credentials(.., true)` 回傳 passphrase、`okx_signature`（Base64 HMAC）、ISO 毫秒時間戳格式化、`is_timestamp_rejected` 納入 `50102`；簽名向量以 Python `hmac`/`base64` 獨立計算並寫入註解；`Cargo.toml` 加 `base64`

## 2. OKX 簽名唯讀客戶端（錄製回應測試）

- [ ] 2.1 fixtures `app/tests/fixtures/okx/signed/`：`account_config_{futures_net,long_short,acctlv4}.json`、`balance_{futures,multi_ccy,empty_availeq}.json`、`positions_{net,isolated,long_short}.json`、`orders_pending_{page1_full,page2_short}.json`、`error_{50102,50105,50011}.json`，`.meta` 註明「依官方文件範例構造、未在真實 demo 帳戶驗證」
- [ ] 2.2 `signed/okx.rs` `OkxSignedClient`：檢查順序（金鑰 → 校時 → 建請求）、`code != "0"` → `Exchange`、`50011/50061` → `RateLimited`、`50102` 重新校時重送一次；`account_mode()` 含 60 秒快取與拒絕規則；測試涵蓋 spec 的金鑰、時間戳、帳戶模式情境
- [ ] 2.3 `get_available_margin()`、`get_balances()`、`get_positions()`（`OkxPosition`，張數）、`get_open_orders()`（`after` 分頁、不完整標示）；測試涵蓋 spec 的保證金、持倉、委託情境；錯誤與 `Debug` 不含機密（以 `redact_secrets` 註冊後斷言）

## 3. 接線

- [ ] 3.1 `execution/account.rs`：`DemoAccountView` 加入 OKX 客戶端，持倉張數、委託 `sz − accFillSz`、保證金；移除 `OKX_ACCOUNT_UNSUPPORTED` 與 `signed/models.rs` 的 `OkxAccount`，更新 `executor_tests.rs` / `models.rs` 的相關測試
- [ ] 3.2 頁面用持倉轉換：`OkxPosition` → `models::Position`（幣量 = 張數 × `ctVal`，`ctVal` 未知標示無法換算）純函式與測試（只建函式，頁面接線在 `okx-trading-enablement`）
- [ ] 3.3 `ui/live.rs`：建立 `OkxSignedClient`（OKX 校時偏移、`ClockResync` 走公開時間端點），加入帳戶輪詢與 `LegAccount` 輪詢迴圈；OKX 的 `AccountState` 推送但總覽 / 持倉頁暫不讀取

## 4. 驗證

- [ ] 4.1 `cargo test -p tong-funding` 與 `cargo test -p tong-funding-core` 全套綠燈；`cargo clippy --all-targets -- -D warnings` 無警告
- [ ] 4.2 實機（使用者執行，agent 不得讀 Keychain 或以真實金鑰發請求）：① 以 `tong-funding secrets set okx api-key|api-secret|passphrase` 寫入 **Demo Trading** 金鑰；② 執行 `cargo test -p tong-funding okx_live_read_probe -- --ignored --nocapture`（本 change 新增的 `#[ignore]` 探針，只發 GET、輸出已遮蔽）；③ 回報 `acctLv`、`posMode`、可用保證金與 OKX demo 網頁是否一致、持倉張數；④ 以回傳內容（去識別化）替換 2.1 的構造 fixtures
