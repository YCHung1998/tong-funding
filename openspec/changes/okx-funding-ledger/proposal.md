## Why

`funding-pnl` 刻意不取 OKX 流水（「OKX 不下單，沒有倉位」，`funding-history-fetch` 第 9 行）：`funding/fetch.rs:234` 跳過 OKX 視窗、`funding/runner.rs:204` 讓 OKX 腿的對帳記為 FAILED、`funding/pnl_record.rs:14-16,163-167,317` 把 OKX 腿的價格當未知、funding 當未取得，`signed/ledger.rs:3` 沒有 OKX 流水客戶端。`okx-demo-execution` 讓 OKX 腿真的會成交後，這些「誠實地未知」會讓每一個含 OKX 的配對 PnL 永遠是 INCOMPLETE。
另外 OKX 成交量是張數，但 PnL 的 `FillRecord.quantity` 定義為幣量（`core/src/pnl.rs:105`），而 `order_fills` 目前直接把張數放進去（`pnl_record.rs:150`）；之前因價格一律未知而被遮住，OKX 一旦有價格就會算錯，必須在同一個 change 修正。

## What Changes

- **OKX 流水客戶端**（`signed/ledger.rs`）：`GET /api/v5/account/bills-archive`（近 3 個月）以 `instType=SWAP`、`instId`、`type=8`、`begin`/`end` 查詢，`after`（billId）分頁、`limit=100`；只保留 `subType` 173（資金費支出）/ 174（資金費收入）的列，金額取 `balChg`（帶號），並以 `subType` 檢查正負號一致（不一致即解析錯誤，fail closed）。請求一律帶模擬交易標頭（`okx-signed-read`）。
- **`LedgerSource` for OKX**：每標的查詢（`per_symbol = true`）、7 天時間窗、無法確認完整即失敗，沿用 `funding/fetch.rs` 既有流程；`runner` 與 `live.rs` 的流水迴圈加入 OKX（OKX 金鑰缺少時略過並記原因，不影響另外兩所）。
- **成交事件記錄合約面值**：engine 的 `ORDER_SUBMITTED` / `ORDER_FILL` 對 OKX 腿多寫 `ct_val`（送單時用於換算的同一個值）。
- **PnL 換算**：`pnl_record` 對 OKX 腿以 `filled_quantity × ct_val` 得到幣量、使用 `avg_price` 與參考價；事件沒有 `ct_val`（本 change 之前的資料）時維持「價格未知、INCOMPLETE」，不猜。OKX 腿的 funding 依取得狀態判定（不再固定 NotFetched）。
- **對帳**：`reconcile_pair` 對 OKX 腿重新抓取並比對，不再一律 FAILED。

## Capabilities

### New Capabilities
- `okx-funding-pnl`: OKX 資金費流水的取得、標準化與去重，OKX 腿成交的幣量換算與 PnL / 對帳納入規則。

### Modified Capabilities
（無已封存的相關 spec。本 change 取代未封存 `funding-pnl` 中 `funding-history-fetch`「OKX SHALL NOT 取得流水」一句與 design 第 8 點「OKX 腿」；該 change 封存時須同步改寫。）

## Impact

- 修改 `app/src/exchange/signed/ledger.rs`、`app/src/funding/{fetch,runner,pnl_record,reconcile}.rs`、`app/src/engine/actor.rs`（成交事件 payload 加 `ct_val`）、`app/src/ui/live.rs`（流水迴圈來源）。
- fixtures：`app/tests/fixtures/funding/okx_bills_funding_*.json`（依文件構造、標註未驗證）。
- 事件 payload 新增欄位（附加，舊事件仍可讀）；不改資料表結構。
- 依賴：`okx-signed-read`（簽名、demo 邊界）、`okx-demo-execution`（OKX 成交事件）。
