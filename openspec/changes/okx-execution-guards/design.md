## Context

來源：`okx-demo-execution` 的對抗審查（red-team）。程式現況見 `okx-demo-execution/design.md`；OKX 事實同該文件（官方文件 2026-10-07）。額外文件事實：Transaction Timeouts 的 `expTime`（伺服器時間超過即不處理下單）；`50004` 文件明說「不代表成功或失敗」；Place order 的 `sz`（SWAP 為張數，必須為 `lotSz` 的整數倍）。

## Goals / Non-Goals

**Goals:** OKX 單在「證明手上是 demo 金鑰、數量合理、結果可判讀」之前不送；環境不符一次即停用；未知不被當成失敗或查無。
**Non-Goals:** 單一實例 DB 鎖（由 orchestrator 另案處理）；UI 橫幅與 `OkxLimitsSource` 的正式接線（`okx-trading-enablement`）；自動拆單；限價單。

## Decisions

**D1　正向證明 = 同一個 `Credentials` 實例的 `GET /api/v5/account/config` 回 `code "0"`，且發生在第一筆 OKX 單之前。** `OkxOrderClient` 持有 `demo_proven` 旗標，由 `position_mode()`（executor 的單向閘門每次送單前都會經過）在收到 `code "0"` 時設為真；`submit` 在旗標為假時直接 `not_sent`、不送請求。因為閘門與送單用同一個客戶端與同一組金鑰，證明與下單不可能分屬不同金鑰。（`50101` 的雙向行為仍是未驗證的推論，見 `okx-signed-read`；本守衛不依賴它，只在它出現時停用。）
**D2　`OkxLatch`（`signed/okx.rs`）：共享的 `Arc`，`trip(reason)` 後永遠為真（本程序）。** 下單客戶端與讀取客戶端都可接上同一個 latch（`with_latch`）。閂鎖後：送單 `not_sent`（原因含 `50101`）、查單 / 撤單 `Failed`、讀取端回 `Exchange{okx_disabled}`。「operator alert」在此以明確原因文字呈現並可由 `latch.reason()` 查詢；畫面橫幅屬 `okx-trading-enablement`（見其 3.6）。
**D3　`OkxLimitsSource`：`fn limits(&self, symbol) -> Option<OkxLimits { ct_val, lot_sz, mark_px, max_leg_notional }>`。** executor 送 OKX 單前呼叫；來源缺、回 `None`、`sz` 不是 `lot_sz` 整數倍、或 `sz × ct_val × mark_px > max_leg_notional` → `not_sent`。工廠預設沒有來源 → OKX 訂單 `not_sent`（fail closed）；正式接線屬 `okx-trading-enablement`。測試涵蓋 `ctVal 1000` 的標的以幣量當張數的情境。
**D4　`expTime`：送單帶 `expTime` 標頭 = 簽名時伺服器時間 + `ORDER_TIMEOUT`。** 客戶端對結果為未知 / 被限流的送單記下 `(clOrdId, expTime)`；之後的查單回 `51603` 時：無紀錄 → 查無（與原行為相同）；有紀錄 → 伺服器時間 ≤ `expTime` 或連續 `51603` 少於兩次 → `Failed`（待確認），之後才 `NotFound`。
**D5　拒絕碼白名單 `OKX_REFUSAL_CODES`：** 只收文件列出且我們理解的碼：`51000`、`51008`、`51010`、`51020`、`51121`、`51131`、`51202`、`51400`、`51603`，以及驗證失敗類 `50102`、`50103`、`50104`、`50105`、`50111`、`50113`（皆表示沒有任何東西被處理）。`50101` 不在其中（它觸發閂鎖，且該次請求的結果歸為未知）。其他代碼（含未列出的 `sCode`）→ 結果未知。`code "0"` 但 `data` 為空或欄位缺漏已是未知（ack 解析失敗）。
**D6　平倉守衛：`reduce_only` 的 OKX 單繞過 executor 的單向快取，每次重讀 `position_mode`。** 平倉被 `51000` / `51010` 拒絕時，`Rejected.message` 加上前綴說明「對側腿裸露，需人工處理」。
**D7　不經 `GatedTransport`：** 靜態檢查 `execution/**` 不出現識別字 `GatedTransport`，且 `ui/live.rs` 建立 `ReqwestOrderTransport` 的工廠直接使用它；`50013` 送單為未知、只送一次（測試）。

## Risks / Trade-offs

- [名目上限來源在本 change 沒有正式接線] → 工廠預設 fail closed：未接線前 OKX 單全部 `not_sent`，合併本 change 不會讓 OKX 單意外放行。
- [`expTime` 對 demo 是否生效是未驗證的] → 探針回報；`expTime` 只收緊判斷，不放寬。
- [白名單可能漏掉真實拒絕碼，使其被當成「未知」] → 未知會以 `clOrdId` 查單確認，最壞是多一次查詢，不會誤判成交或重送。

## Migration Plan

純新增守衛；OKX 單在 `okx-trading-enablement` 接上 `OkxLimitsSource` 前一律 `not_sent`。回復：移除守衛即回到 `okx-demo-execution`。

## Open Questions

1. 單腿名目上限使用哪個設定值（`okx-trading-enablement` 決定）。
2. demo 是否實際接受 `expTime` 標頭（探針）。

## 實作時發現

- 「operator alert」目前只有原因文字（`not_sent` 訊息含 `50101`、`OkxLatch::reason()` 可查）；畫面橫幅與共用 latch 的接線列入 `okx-trading-enablement` 3.6。engine 對失敗平倉本來就進 alert 狀態，裸腿文字會出現在其原因中。
- 合約測試的 OKX 參數化（`contract_tests.rs`）需要 `expire_unknown_submits()`：OKX 的「從未到達」要等 `expTime` 過後兩次查無，與 Binance / Bybit 的立即查無不同，這是 D4 的刻意差異。
- 讀取端 `OkxSignedClient` 的 latch 測試與實作同時寫（紅燈證據只涵蓋下單端與 executor）。
- `clippy -D warnings` 在 baseline 的 `tong-funding-core` 即失敗（既有問題，與本計畫無關）。

## 抗辯修正（第二輪）

- R1：平倉（`reduce_only`）只檢查 `sz > 0` 與（lotSz 已知時的）整數倍，**永不**因名目上限或缺標記價 / limits 而被擋；`sz ≤ 已知持倉張數` 的檢查需要持倉資料，executor 沒有，未實作（列為後續）。
- R2：開倉必須帶 `intended_base_qty`，且 `|sz × ctVal − intended| ≤ lotSz × ctVal`，否則 `not_sent`；缺 `intended_base_qty` 也不送（`0.01` BTC 當作 `sz` 的情境被擋）。
- R3：`51603` 判為查無需 `now ≥ expTime + 2 s` 且兩次確認相隔 ≥ 1 s（以注入時間計）；**未持久化**：order intents 沒有 `expTime` 欄位，不為此改 schema。保守規則：重啟 / 工廠重建後沒有紀錄的 id，`51603` **永不**判為查無，維持「待確認」；只有本程序內被明確拒絕（Rejected）的送單才立即信 `51603`。這使恢復流程對 OKX 腿在重啟後無法以查無收斂（需人工 / 持倉比對），是刻意的 fail-closed。
- R4：`50101` 出現在送單回應 = 閘道在處理前拒絕 → `Rejected{50101}`（並閂鎖）；閂鎖後送單與帳戶模式讀取被擋，**查單與撤單不被擋**（它們只收斂可能已掛上的單，不會增加曝險）。
- R5：工廠擁有唯一的 `Arc<OkxLatch>`（`okx_latch()`），建出的每個 executor 與其 OKX 下單客戶端共用；讀取端的接線（`OkxSignedClient::with_latch(factory.okx_latch())`）屬 `okx-trading-enablement` 3.5。`OkxLatch` 改為 `OnceLock<String>`，原因文字為單一常數 `ENV_MISMATCH_REASON`。
- R6：任何被拒絕的 OKX 平倉（含送出前的 `not_sent`）原因都帶「對側腿裸露」文字；開倉不帶。
- R9：`live_probe` 對「成交未知」的腿讀帳戶持倉並平掉非零持倉；連持倉都讀不到時印出醒目的手動平倉指示，不再默默略過。

## 抗辯修正（第三輪）

- P1：`live_probe` 的開倉單由純函式 `probe_request` 建構，OKX 開倉帶 `intended_base_qty`；探針在送出任何一腿之前先以 `check_size` 驗證 OKX 腿，被本地拒絕就不開另一所的腿（避免單邊曝險）。
- P2：閂鎖只有一個來源：工廠建立 OKX 客戶端時直接 `.with_latch(factory.okx_latch())`；executor 不再有自己的 latch 欄位 / `with_okx_latch` / `okx_latch()`。
- P3：`pending` / `refused` 紀錄有界：每次插入時丟棄超過 `expTime + 10 分鐘` 的紀錄，且每個表上限 1000 筆（超過丟最舊）。
- P4：移交 `okx-trading-enablement` 3.7。
