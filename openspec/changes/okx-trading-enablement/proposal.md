## Why

前三個 change（`okx-signed-read`、`okx-demo-execution`、`okx-funding-ledger`）完成後，OKX 在 engine 層已能讀帳戶、下單與算 PnL，但頁面仍把 OKX 當「僅比價」：可下單集合寫死 Binance + Bybit（`ui/vm/bridge.rs:29-36`），掃幣頁的達標與方向排除 OKX（`ui/vm/scanner.rs:3,409,497,513`），候選勾選擋下含 OKX 的列（`ui/vm/candidates.rs:19,36,70`），手動下單頁沒有 OKX 面板（`ui/vm/manual_order.rs:57,151,165-166`），總覽是一張固定的「僅比價」卡（`ui/vm/dashboard.rs:17,245`、`ui/pages.rs:230`），持倉頁固定註明「OKX 僅比價，不顯示持倉」（`ui/vm/positions.rs:21,343`）。本 change 讓使用者在頁面上對 OKX 擁有與 Binance、Bybit 相同的能力。

## What Changes

- **可下單集合**：`TRADABLE_EXCHANGES` 與 `ACCOUNT_EXCHANGES` 改為三所；`is_tradable` 對 OKX 為真。
- **掃幣頁**：達標判定、Gross Spread、Net Edge 與「最佳套利方向」在三所之間計算（受 `allowed_exchanges` 限制）；OKX 儲存格不再帶「僅比價」。
- **候選勾選**：移除「OKX 僅比價」阻擋；含 OKX 的列依一般規則（達標、規則與價格已知）決定可否勾選。
- **手動下單頁**：新增 OKX 面板；使用者輸入**幣量**，以 `ctVal` 與 `lotSz` 換算成張數並顯示「N 張（≈ x BTC，≈ y USDT）」；持倉選擇器帶入的 OKX 數量以張數 × `ctVal` 顯示為幣量。
- **交易單頁**：OKX 腿的可用保證金列；與進行中的 `trade-cost-estimate` 銜接——OKX 一檔掛單量為張數，比較前 SHALL 先乘 `ctVal`。
- **總覽**：OKX 卡改為真實帳戶卡（資產、合約權益、占比），未連線 / 讀取失敗 / 帳戶模式不支援時以對應狀態呈現且不計入總額；移除「僅比價」說明卡。
- **持倉頁**：顯示 OKX 持倉（張數與換算幣量）；移除「OKX 僅比價」註記；`ctVal` 未知時顯示「無法換算」。
- **BREAKING（行為）**：OKX 開始參與掃幣方向與自動候選；使用者若不想交易 OKX，須在風控設定把 OKX 移出 `allowed_exchanges`（見 design 的遷移）。

## Capabilities

### New Capabilities
- `okx-trading-ui`: OKX 在掃幣、候選、手動下單、交易單、總覽、持倉頁與 Binance / Bybit 對等的呈現與操作規則，包含張數與幣量的顯示換算。

### Modified Capabilities
（無已封存的相關 spec。本 change 取代下列未封存 change 中的 OKX 排除條款，各 change 封存時須同步改寫：`ui-readonly-pages` 的 `scanner-page`「OKX 僅供比價，不參與達標與方向」、`dashboard-page` OKX 說明卡、`positions-page` OKX 註記；`ui-trading-pages` 的 `scanner-candidates`「含 OKX 的列不可勾選」、`manual-order-page`「只提供 Binance 與 Bybit」。）

## Impact

- 修改 `app/src/ui/vm/{bridge,scanner,scan_view,candidates,manual_order,staged_orders,dashboard,positions}.rs`、`app/src/ui/{pages,trading_pages,live}.rs` 與對應 `*_tests.rs` / UI 測試。
- 依賴：`okx-signed-read`（帳戶資料）、`okx-demo-execution`（下單）、`okx-funding-ledger`（PnL 完整，建議先完成）、`trade-cost-estimate`（另一位 agent 進行中；本 change 在其合併後才開始實作）。
- 不改 engine 的風控與送單邏輯（engine 早已以 `allowed_exchanges` 與張數處理 OKX）。
