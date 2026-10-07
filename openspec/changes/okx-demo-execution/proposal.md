## Why

`DemoExecutor` 對 OKX 一律回「not_sent: OKX order execution is unsupported」（`execution/executor.rs:31-33,132-205`），執行器工廠要求 Binance 與 Bybit 金鑰、完全不考慮 OKX（`execution/factory.rs:1-6,55-66`），靜態檢查更明文禁止 `execution/` 出現 OKX 請求（`static_checks.rs:871-891`）。engine 的數量、成交比對與恢復早已以張數處理 OKX（`engine/fill.rs:23-55`、`engine/recovery.rs:716-723`），缺的只是一個真的能在 OKX demo 送單、查單、撤單的執行器。`okx-signed-read` 提供了 demo 邊界、簽名與帳戶模式檢查，本 change 在其上補齊下單。

## What Changes

- **OKX 下單客戶端**（`execution/okx.rs`）：`POST /api/v5/trade/order`（`instId=<BASE>-USDT-SWAP`、`tdMode=cross`、`side`、`ordType=market`、`sz`=張數、`clOrdId`=engine id、`reduceOnly`；不帶 `posSide`）、`GET /api/v5/trade/order`（以 `clOrdId` 或 `ordId` 查詢）、`POST /api/v5/trade/cancel-order`（只限 `order_intents` 中的 id，撤後回讀）。所有請求的網址只能經 `okx-signed-read` 的 `OkxHost::target()` 取得，並無條件帶 `x-simulated-trading: 1`。
- **結果分類**：`code`/`sCode` 雙層判讀；`50001/50004/50013/50026`、逾時、無法解析 → 結果未知；`50011/50061` → 被限流；`51603` → 查無；其他明確代碼 → 已拒絕；`50102` → 重新校時後再判斷（不重送下單，見 design）。
- **持倉模式閘門**：送單前沿用 `okx-signed-read` 的帳戶模式讀數（`acctLv` 2/3 且 `net_mode`），不符即不送（`not_sent`）。
- **`clOrdId` 檢查**：OKX 只接受英數字、≤ 32 字元；不符即不送（engine id 已是小寫英數 ≤ 31，正常路徑不受影響）。
- **手續費與單位**：OKX `fee` 為負代表支出，轉為 engine 的「正數 = 支付」；`feeCcy` 原樣；`accFillSz` 為張數，直接對應 `OrderStatus.filled_quantity`（張數契約不變）。
- **執行器與工廠**：`DemoExecutor` 加入可選的 OKX 客戶端；工廠在 OKX 金鑰（含 passphrase）齊全時建立，缺少時 OKX 訂單 `not_sent`「OKX 金鑰不可用」，Binance / Bybit 仍為必要（行為不變）。
- **靜態檢查演進**：`execution/` 允許 OKX 下單客戶端，但仍禁止任何主機字面與 `x-simulated-trading` 字面（標頭字面只在 `signed/endpoints.rs`）。
- **使用者實機探針**：`live_probe.rs` 支援 OKX 腿（`TONG_DEMO_EXCHANGES`），只在使用者設定確認環境變數時送出 demo 單。
- 不改頁面可下單集合（`okx-trading-enablement`）；engine 層已接受 OKX，但 UI 與候選勾選仍擋下。

## Capabilities

### New Capabilities
- `okx-order-execution`: OKX demo 的送單 / 查單 / 撤單、結果分類、`clOrdId` 與張數規則、手續費正負號、執行器與工廠的 OKX 金鑰處理。

### Modified Capabilities
（無已封存的相關 spec。本 change 取代未封存 `exchange-demo-execution` 中 `signed-order-execution`「OKX SHALL NOT 有簽名端點，也 SHALL NOT 下單」與「沒有 OKX 下單能力」情境，並補足「每一單帶 client order id」的 OKX 參數名；該 change 封存時須同步改寫。）

## Impact

- 新增 `app/src/exchange/execution/okx.rs`；修改 `execution/{executor,factory,classify,endpoints,http,mod,live_probe}.rs`、`exchange/static_checks.rs`、`ui/live.rs`（`demo_keys` 不再要求 OKX，工廠傳入 OKX 選項）。
- 錄製回應 fixtures：`app/tests/fixtures/okx/orders/*.json`（依文件構造，標註未驗證）。
- 依賴：`okx-signed-read`（簽名、demo 邊界、帳戶模式、`Credentials.passphrase`）。
