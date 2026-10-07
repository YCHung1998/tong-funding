## Why

OKX 目前只有公開行情：帳戶查詢一律回「不支援」（`execution/account.rs:18` `OKX_ACCOUNT_UNSUPPORTED`、`signed/models.rs:100` `OkxAccount`），也沒有 `signed/okx.rs`。Binance 與 Bybit 已有簽名唯讀客戶端、engine 的 `AccountView`、帳戶輪詢與保證金檢查；OKX 要能下單、算 PnL、在頁面上與另外兩所對等，第一步必須先能「安全地讀」OKX demo 帳戶。
當初排除 OKX 的理由（`exchange-readonly-adapters` D8：Python 版 OKX 簽名從未在真實 demo 帳戶驗證成功；OKX demo 與正式「同主機、靠 header 區分」，無法套用「只准 demo 主機」的防線）仍然成立，所以本 change 的重點是補上一條等價的 demo 邊界，而不是只照抄 Bybit。

## What Changes

- **OKX demo 邊界**：新增 OKX 簽名主機常數（`signed::endpoints`，文件 2026-10 版 REST 主機 `openapi.okx.com`），每個 OKX 簽名請求在建構時一律帶 `x-simulated-trading: 1`，無法省略或覆寫；傳輸層對 OKX 主機缺少該標頭的請求拒絕連線；靜態檢查改為「OKX 主機與 `x-simulated-trading` 字面只准出現在 `signed/endpoints.rs`」，且取得 OKX 網址的唯一 API 同時回傳該標頭。
- **簽名**：`OK-ACCESS-KEY / SIGN / TIMESTAMP / PASSPHRASE`；`SIGN = Base64(HMAC-SHA256(secret, timestamp + METHOD + requestPath(含 query) + body))`；時間戳為校時後的 ISO 8601 UTC 毫秒字串。`Credentials` 增加 passphrase（`load_credentials(.., require_passphrase = true)`，缺少即 `NoPassphrase`，不送任何請求）。
- **帳戶設定檢查**：`GET /api/v5/account/config` 讀 `acctLv` 與 `posMode`；只接受 `acctLv` 2（合約模式）或 3（跨幣種保證金）且 `posMode = net_mode`，否則帳戶資料標示「帳戶模式不支援」而不是空資料。
- **唯讀查詢**：餘額（`/api/v5/account/balance`）、可用保證金（依 `acctLv` 取 USDT `availEq` 或帳戶層 `availEq`，空值即錯誤）、持倉（`/api/v5/account/positions?instType=SWAP`，數量為**張數**，另附 base 幣量供頁面使用）、未成交委託（`/api/v5/trade/orders-pending?instType=SWAP`，以 `after` 分頁至取完，否則標示不完整）。
- **engine 帳戶介面**：`DemoAccountView` 的 OKX 分支改走新客戶端（持倉為張數，符合 `AccountPosition` 既有單位契約）；`OKX_ACCOUNT_UNSUPPORTED` 與 `OkxAccount` 移除。
- **輪詢接線**：`ui/live.rs` 的帳戶輪詢與 `LegAccount` 輪詢加入 OKX（只讀；頁面呈現在 `okx-trading-enablement`）。
- 不下單、不取流水、不改頁面可下單集合（見 design 的路線圖）。

## Capabilities

### New Capabilities
- `okx-signed-read`: OKX demo 簽名唯讀存取——demo 邊界（主機 + 模擬交易標頭）、passphrase 金鑰、簽名與時間戳、帳戶模式檢查、餘額 / 可用保證金 / 持倉 / 未成交委託的標準化與失敗處理。

### Modified Capabilities
（無已封存的相關 spec。本 change 取代尚未封存的 `exchange-readonly-adapters` 中 `signed-read-access`「OKX SHALL NOT 有任何簽名請求的實作」一句；該 change 封存時須同步改寫，見 design「與既有 spec 的關係」。）

## Impact

- 新增 `app/src/exchange/signed/okx.rs`；修改 `signed/{endpoints,signing,models,mod}.rs`、`reqwest_transport.rs`（`HostPolicy::SignedDemo` 與標頭檢查）、`exchange/static_checks.rs`、`execution/account.rs`、`ui/live.rs`。
- 新依賴：`base64`（目前只是間接依賴）。
- 錄製回應 fixtures：`app/tests/fixtures/okx/signed/*.json`（依官方文件範例構造，`.meta` 註明來源與未驗證）。
- 金鑰：沿用 `store/secrets.rs:104` 已支援的 OKX `api_key / api_secret / passphrase`，不改儲存格式。
