## Context

### 現況（程式證據）

- `execution/executor.rs:31-33`：`OKX_UNSUPPORTED`；`submit_classified`、`query_by`、`cancel_own`、`ensure_one_way` 對 OKX 一律不送（`:117,132,152,164,176,183,204`）。
- `execution/factory.rs:48-66`：只讀 Binance、Bybit 金鑰（`load_credentials(.., false)`），兩者缺一即 `Err`，engine 留在 SIMULATION。
- `execution/http.rs:36-48`：`DemoEnv` 只有 `Binance`、`Bybit`；`ReqwestOrderTransport` 以 `HostPolicy::SignedDemo` 擋主機。
- `execution/order.rs:10-31`：`ClientOrderId` 接受 `[A-Za-z0-9_-]`、≤ 36、必須是 `demo` 前綴；`engine/ids.rs:5-8` 實際產生的是小寫英數、≤ 31，刻意落在 OKX `clOrdId` 限制內。
- `execution/classify.rs:23-33`：Binance / Bybit 的未知、限流、查無代碼表；無 OKX。
- `engine/ports.rs:57-90`：`OrderRequest.quantity`、`OrderStatus.filled_quantity` 對 OKX 一律為張數；`fee` 正數 = 支付。
- `engine/fill.rs:49-55,84-86`、`engine/recovery.rs:716-723`：OKX 張數與 `ctVal` 換算已存在並有測試。
- `static_checks.rs:871-891`：`execution/` 不得有任何 OKX 識別字（`Okx` 變體與兩個 `*_UNSUPPORTED` 常數除外）。
- `live_probe.rs:55-60,127-185`：使用者在 Mac 上以 `TONG_DEMO_LIVE` 確認後才送 demo 單；OKX 偏移固定 `None`。

### OKX API v5 事實（https://www.okx.com/docs-v5/en/ ，2026-10-07 取得）

- **Trade → POST / Place order**（60 次 / 2 秒，User ID + Instrument ID；另受子帳戶 1000 次 / 2 秒限制，超過回 `50061`）：`instId`、`tdMode`（`cross` / `isolated`）、`side`（`buy` / `sell`）、`posSide`（net 模式預設 `net`，long/short 模式必填，否則 `51000`）、`ordType=market`（適用 SWAP）、`sz`（SWAP 為張數）、`clOrdId`（「case-sensitive alphanumerics, all numbers, or all letters of up to 32 characters」）、`reduceOnly`（「Only applicable to Futures mode and Multi-currency margin」，net 模式）。回應 `code`/`msg` 與 `data[0].{ordId, clOrdId, ts, sCode, sMsg, subCode}`；**General Info**：有 `sCode` 時以 `sCode`/`sMsg` 表示結果。
- **Trade → GET / Order details**（60 次 / 2 秒）：`ordId` 或 `clOrdId` 擇一（同時給以 `ordId` 為準；同一 `clOrdId` 對到多筆只回最新）；欄位 `state`（`live`、`partially_filled`、`filled`、`canceled`、`mmp_canceled`）、`accFillSz`（SWAP 為張數）、`avgPx`、`fee`（負數 = 平台收取）、`feeCcy`。
- **Trade → POST / Cancel order**（60 次 / 2 秒）：`instId` + `ordId` 或 `clOrdId`；回應同樣 `sCode`/`sMsg`；`51400` = 已成交 / 已撤 / 不存在而撤單失敗。
- **Trade → GET / Order history (last 7 days)**：「The incomplete orders that have been canceled are only reserved for 2 hours」。
- **Transaction Timeouts**：可帶 `expTime` 標頭，伺服器時間超過即不處理下單。
- **Error Codes**：`51603` 訂單不存在、`51000` 參數錯誤、`51010` 目前帳戶模式不支援、`51121` 數量非 lot 整數倍、`51131` 餘額不足、`51202` 市價單數量超過上限、`50001` 服務暫不可用、`50004` 端點逾時（「does not mean that the request was successful or failed」）、`50013` 系統忙碌、`50026` 系統錯誤、`50011` 限流、`50061` 子帳戶限流、`50102` 時間戳過期、`50101` 環境不符。

## Goals / Non-Goals

**Goals:**
- engine 在 EXCHANGE_DEMO 下能對 OKX demo 送出、查詢、撤銷自己的市價單，結果分類與 Binance / Bybit 同一套契約（`executor_tests.rs` 的介面契約測試涵蓋 OKX）。
- 未知結果不被當成失敗：一律以 `clOrdId` 查單確認。
- OKX 金鑰缺少不影響現有的 Binance + Bybit demo 交易。

**Non-Goals:**
- 頁面的 OKX 下單入口、候選勾選、掃幣方向（`okx-trading-enablement`）。
- 限價單、批次下單、改單、`expTime`、WebSocket 下單、設定槓桿或帳戶模式。
- long/short 模式（`posSide`）、逐倉、組合保證金。

## Decisions

**D1　市價單、`tdMode=cross`、不帶 `posSide`、明確帶 `reduceOnly`。**
與 Bybit（`positionIdx` 0、`reduceOnly`）對等。`okx-signed-read` 已保證 `net_mode` 與 `acctLv` 2/3，在此前提下省略 `posSide` 即為 `net`，`reduceOnly` 有效。
- 替代：`tdMode=isolated`。需要先轉入保證金且與 `okx-signed-read` 拒絕逐倉持倉的決定矛盾。

**D2　`clOrdId` 直接用 engine id，OKX 端另加「英數 ≤ 32」檢查。**
`ClientOrderId::parse` 的通用規則允許 `_`、`-` 與 36 字元；OKX 建構時再檢查一次，不符 → `not_sent`（確定未送出）。engine 實際 id 恆通過。
- 替代：收緊 `ClientOrderId` 全域規則。會影響 Binance / Bybit 既有測試與 spec，非必要。

**D3　分類：先看 HTTP / 傳輸，再看 `code`，再看 `data[0].sCode`。**
- 傳輸失敗、逾時、5xx、無法解析、`code` 或 `sCode` 為 `50001/50004/50013/50026` → 結果未知。
- `code` 或 `sCode` 為 `50011/50061`、HTTP 429 → 被限流（engine 視為未知並查單確認，與 `classify.rs:56-62` 相同）。
- `code = "0"` 且 `sCode = "0"` → 已接受（`ordId`）。
- `code` 為 `"1"` 且 `sCode` 為其他明確代碼（如 `51131`、`51121`、`51010`）→ 已拒絕。
- 查單 / 撤單：`51603` → 查無；撤單 `51400` → 回讀訂單後決定。
- `50102`（時間戳過期）在**送單**時歸為已拒絕（OKX 明確未處理），由 engine 依既有規則記為 FAILED；查單 / 撤單則沿用「重新校時、重試一次」。
- 替代：送單也自動重試一次。違反「不在執行器內重送」的既有原則（`signed-order-execution`：未知結果以同一 id 查單，不重送）。

**D4　手續費正負號在客戶端轉換。**
`OrderStatus.fee = −fee`（OKX 負數 = 支付，engine 正數 = 支付）；`feeCcy` 空字串 → `None`。
- 替代：交給 PnL 層處理。會讓 `OrderStatus` 在不同交易所有不同語意，違反 `ports.rs:79-81` 的契約。

**D5　工廠：OKX 金鑰可選，Binance / Bybit 維持必要。**
`DemoExecutor` 持有 `Option<OkxOrderClient>`；缺金鑰時 OKX 訂單 `not_sent`（原因含 `NoKey`/`NoSecret`/`NoPassphrase`）。理由：不讓沒有 OKX 帳戶的使用者失去現有能力；engine 的 `allowed_exchanges` 與後續 UI 決定是否會產生 OKX 訂單。
- 替代 A：OKX 也必要。會讓既有使用者無法進入 EXCHANGE_DEMO。
- 替代 B：依 `allowed_exchanges` 決定必要集合。工廠目前拿不到設定，且設定可在執行中改變；列為 Open Question。

**D6　查單只打 `GET /api/v5/trade/order`，不另查歷史端點。**
文件的 Order details 涵蓋已完成訂單；Bybit 需要 `realtime → history` 兩段（`execution/bybit.rs:121-131`），OKX 不需要。

**D7　靜態檢查：`execution/` 允許 `okx` 識別字與 `/api/v5/trade/` 路徑常數（只在 `execution/endpoints.rs`），仍禁止任何主機字面與 `x-simulated-trading` 字面。**
`DemoEnv` 新增 `Okx(OkxHost)`；`OrderHttpRequest::to_demo` 對 OKX 只能經 `OkxHost::target()`（`okx-signed-read` D1）取得網址，並無條件套用其回傳的模擬交易標頭。錄製測試斷言每個 OKX 送單、查單、撤單請求都帶該標頭；`ReqwestOrderTransport` 也做與 `okx-signed-read` 相同的缺標頭零連線檢查。

## Risks / Trade-offs

- [`50004` 逾時文件明說「不代表成功或失敗」] → 歸為未知並以 `clOrdId` 查單，絕不重送。
- [未成交即撤銷的訂單只保留 2 小時；重啟後查單得到 `51603`] → 市價單通常立即成交；查無在 engine 既有恢復規則下等同「沒有成交」。部分成交後撤銷的訂單是否也只保留 2 小時，文件不明確，列為實機驗證項目；在確認前，OKX 腿的恢復若遇查無而帳戶有持倉，engine 依既有規則轉人工（不猜）。
- [市價單數量超過上限 `51202`、或價格限制帶] → 已拒絕，engine 依部分失敗流程處理；不自動拆單。
- [OKX 下單限流按 instId 計，與 Binance 權重不同] → 共用 `RateLimiter` 的 Signed 類別與退避；兩腿並行、每腿一單，遠低於 60 次 / 2 秒。
- [demo 環境與正式行為差異（成交價、手續費）] → 實機探針記錄成交、手續費與 `ctVal` 換算，由使用者核對 OKX demo 網頁。

### OKX 特有陷阱

- `sz` 是**張數**，不得把 base 幣量送給 OKX（`ports.rs:57-60` 已是契約；本 change 以錄製測試斷言請求本文的 `sz` 等於 `Quantity` 的張數字串）。
- `instId` 須為 `BTC-USDT-SWAP`，不是 `BTCUSDT`（沿用 `public/okx.rs:60-63` 的轉換規則，但 execution 不得 import `public`：在 execution 內以純函式重寫並以相同測試向量鎖定）。
- `posSide`：long/short 模式省略即 `51000`；系統在 `okx-signed-read` 已拒絕該模式，送單前再次確認讀數未過期。
- `reduceOnly` 只在合約模式與跨幣種保證金有效；組合保證金已被拒絕。
- 回應永遠是 HTTP 200 + `code`，真正結果在 `data[0].sCode`；只看 HTTP 或只看 `code` 都會誤判。
- 手續費正負號與另外兩所相反。

## Migration Plan

- 本 change 合併後，engine 可產生 OKX 訂單的唯一途徑是 `allowed_exchanges` 含 OKX 且頁面放行；頁面放行在 `okx-trading-enablement`，因此合併本 change 本身不改變使用者可見行為（手動下單頁仍無 OKX 面板、候選仍擋 OKX）。
- 回復：工廠不建 OKX 客戶端即恢復「not_sent」。

## Open Questions

1. 工廠是否改為「`allowed_exchanges` 中每一所的金鑰都必須存在」（D5 替代 B）？
2. OKX demo 的市價單是否在 ACK 時已成交（`Place order` 回應不含成交量，需查單）——探針確認首次查單的狀態分布，用以調整成交輪詢。
3. 部分成交後撤銷的訂單在 OKX 保留多久（Risk 第二點）？
