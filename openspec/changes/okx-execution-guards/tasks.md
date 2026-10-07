## 1. 守衛（先寫失敗測試）

- [x] 1.1 `signed/okx.rs` `OkxLatch`；`execution/okx.rs` `demo_proven` 與 `50101` 閂鎖（送單 / 查單 / 撤單 / 讀取端）；測試涵蓋 spec 的證明與閂鎖情境
- [x] 1.2 `OkxLimitsSource` 與 executor 的數量守衛（`lotSz` 整數倍、名目上限、來源缺即不送）；測試含 `ctVal 1000` 幣量誤當張數
- [x] 1.3 `expTime` 標頭與雙重 `51603`（含 `50004` 後立即 `51603` 維持待確認）
- [x] 1.4 `classify.rs` 拒絕碼白名單；fixtures `place_code0_empty_data`、`place_scode_unlisted`、`place_50101`
- [x] 1.5 平倉守衛：`reduceOnly` 不用快取、`51000/51010` 裸腿原因
- [x] 1.6 不經 `GatedTransport` 的靜態檢查（`execution/**` 與 `ui/live.rs`），`50013` 單次送出測試

## 2. 實機探針與驗證

- [x] 2.1 `live_probe.rs`：`TONG_DEMO_EXCHANGES`、OKX 校時、以公開 `ctVal` 把幣量換成張數並同時印出兩種單位（只編譯，不執行）
- [x] 2.2 `cargo test -p tong-funding`、`cargo test -p tong-funding-core`、`cargo build` 全綠
- [ ] 2.3 實機（使用者執行；agent 不得讀 Keychain、不得送單）：`TONG_DEMO_LIVE=<確認字串> TONG_DEMO_EXCHANGES=bybit,okx TONG_DEMO_SYMBOL=ETHUSDT TONG_DEMO_QTY=<幣量> cargo test -p tong-funding live_demo_probe -- --ignored --nocapture`；回報：`expTime` 是否被 demo 接受、ACK 後首次查單的 `state`、成交張數 × `ctVal` 是否等於另一腿幣量、並確認以正式環境金鑰帶模擬標頭 / demo 金鑰各回什麼代碼（`50101` 的雙向行為，只查文件，不得以程式送出缺標頭的請求）

## 3. 第二輪抗辯修正

- [x] 3.1 R1–R9、S1–S3 與簡化項（見 design「抗辯修正（第二輪）」；測試涵蓋、變異檢查為紅燈）
