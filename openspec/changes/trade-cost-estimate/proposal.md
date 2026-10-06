## Why

兩腿已固定為同一幣數（`matched-leg-quantity`），每腿的成交價值與手續費因此可以事先估算。目前交易單頁只顯示數量與以 mark price 算的價值，看不到「實際要付多少」：多腿以賣一價買入、空腿以買一價賣出，再加上兩腿各自的 taker 手續費。
同時，送單前的保證金檢查只用 `notional ÷ leverage`，沒有計入手續費。Bybit 可用保證金修正後，Bybit 腿第一次能通過這個檢查，所以這個缺口現在真的會發生。

## What Changes

- **最佳一檔報價（買一 / 賣一的價與量）**：Bybit 與 OKX 從已在輪詢的 tickers 回應中多解析這些欄位，不增加請求；Binance 在同一輪 10 秒輪詢中並行加抓 `/fapi/v1/ticker/bookTicker`（全市場一次）。
- **成本估算純函式（core）**：輸入固定數量、多腿賣一價、空腿買一價、兩腿 taker 費率與槓桿，輸出每腿預估成交價值、開倉手續費、預估平倉手續費、每腿所需保證金與合計成本；數量大於該檔掛單量時標示「會吃到第二檔」（只提示，不阻擋）。
- **交易單頁滾動更新**：每列顯示上述估算與報價的資料時間，隨行情快照更新；計算在背景完成，不影響送單。
- **送單前保證金需求計入手續費**：每腿所需保證金 = `數量 × 該腿送單前最新價 ÷ 槓桿 + 開倉手續費`（取代 `notional ÷ leverage`）。只用送單前已抓到的資料，不在送單路徑上增加任何請求。
- **手續費率來源**：沿用風控設定的 `taker_fee_pct`（使用者依帳戶等級填寫，已有「事實值」標記），不在送單路徑上查詢交易所費率 API。

## Capabilities

### New Capabilities
- `trade-cost-estimate`: 最佳一檔報價、固定數量下的成本與手續費估算、交易單頁滾動顯示。

### Modified Capabilities
- `pretrade-validation`: 新增「所需保證金」的定義（計入開倉手續費）。

## Impact

- `app/src/exchange/public/{binance,bybit,okx}.rs`、`endpoints.rs`：解析 / 抓取買一賣一；`FundingObservation` 或行情快照新增最佳一檔欄位。
- `core/src/`：新增成本估算純函式與測試。
- `app/src/engine/node0.rs`：`margin_needed` 改用固定數量與手續費。
- `app/src/ui/vm/staged_orders.rs`、`app/src/ui/trading_pages.rs`：顯示估算。
- 不在本 change：交易所槓桿與設定槓桿不一致的檢查、同時進入送單前檢查的多組配對互相保留保證金（見 design 的後續項目）。
