## Why

沒有真實資料，後面的頁面與引擎都無法驗證。這個 change 提供**唯讀**的交易所存取：公開行情（含 funding 週期）與簽名的 GET（餘額、持倉、委託），
並把 Python 版已確認的缺陷一併修掉：價格資料沒有過期檢查、批次端點未檢查分頁 cursor 會被靜默截斷、本機時鐘未與交易所校時、遇到 429 沒有退避，
以及查證時新發現的 OKX 成交量單位錯誤（`volCcy24h` 是幣量，不是 USDT）。

## What Changes

- `ExchangeAdapter` trait（只有讀取方法）、標準化輸出（`FundingObservation`、`InstrumentRules`、帳戶餘額、持倉、委託）與封閉的 `AdapterError`。
- Binance 公開行情（`premiumIndex`、`fundingInfo`、`exchangeInfo`、24h tickers、WebSocket `!markPrice@arr@1s`）、Bybit 公開行情（`tickers`、`instruments-info` 的 `fundingInterval`、分頁 cursor 檢查）、OKX 公開行情（`funding-rate`、`instruments` 的 `ctVal`、`tickers`）。
- 單一標的重新抓取（送單前檢查與「立即刷新」使用，不經任何快取）。
- Binance、Bybit 的簽名 GET（餘額、持倉、未成交委託）；OKX 簽名端點**不做**（Python 版從未在真實 demo 帳戶驗證成功，HANDOFF Fragility #6）。
- `serverTime` 校時（偏移量供簽名與之後的排程使用）、429 與 `Retry-After` 退避、WebSocket 斷線偵測與資料新鮮度判定、健康狀態轉換才寫入事件。
- 公開客戶端與簽名客戶端在型別上分離；簽名請求的主機為編譯期常數，只含 demo/testnet，程式中不存在把簽名請求送往正式環境的路徑。
- 以錄製的回應做測試，並對真實 demo 帳戶做一次讀取驗證。

**不在本 change 範圍**：下單與撤單（`exchange-demo-execution`）、OKX 簽名端點、funding 收付流水（`funding-pnl`）、任何 UI。

## Capabilities

### New Capabilities
- `exchange-adapter`: 唯讀的統一 adapter 介面與標準化輸出、各所公開行情、funding 週期與結算時間的取值與一致性檢查、上市過濾、分頁完整性、成交量單位、單一標的重新抓取、標的規則。
- `signed-read-access`: Binance 與 Bybit 的簽名 GET、端點寫死為 demo/testnet、金鑰取得失敗時視為未連線、簽名時間戳使用校時後的時間。
- `feed-health`: 時鐘校時、限流退避、Binance WebSocket 接收與斷線、資料來源新鮮度判定、快取讀取介面、健康狀態轉換的事件記錄。

### Modified Capabilities
<!-- 無 -->

## Impact

- `app` 新增依賴：HTTP 客戶端、WebSocket 客戶端、tokio（選型見 design.md D9，尚未編譯驗證）。這會改變 `bootstrap-gpui-shell` task 1.4 記錄的「`app` 沒有網路依賴」狀態。
- 依賴 `core-domain-and-fixtures`（型別）與 `store-sqlite`（Keychain、遮蔽函式、事件寫入）。
- 提供給 `ui-readonly-pages`（資料與健康狀態）、`engine-simulation`（校時偏移量、單一標的重新抓取）與 `exchange-demo-execution`（簽名客戶端的基礎）。
