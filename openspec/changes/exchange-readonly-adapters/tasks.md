測試指令（`app` 的 package 名稱為 `tong-funding`，`core` 為 `tong-funding-core`）：`cd /Users/eason.hung/Documents/github/tong-funding && cargo test -p tong-funding exchange::`。
標示「先紅後綠」的 task，要先寫測試並貼出失敗輸出（失敗原因須是預期的），再實作到通過。

## 1. Adapter 骨架

- [ ] 1.1 加入 HTTP、WebSocket、tokio 依賴；定義 `ExchangeAdapter`（唯讀）、`AdapterError`、`Clock` trait、公開與簽名兩種客戶端型別、endpoint 常數模組（正式主機只在公開模組）。先紅後綠：靜態掃描測試（簽名模組無正式主機、無 base URL 建構函式、只有 GET）。驗收：該測試通過；`cargo tree -p tong-funding` 記錄新增依賴；若 `cargo build` 失敗，停止並回報，不硬改（D9）
- [ ] 1.2 校時純函式與狀態（偏移量、RTT、未校時、沿用舊偏移量、被拒絕後重校一次）與限流退避狀態機（`Retry-After`、指數退避、成功重設、Binance 權重 80% 暫緩）。先紅後綠：每條 `feed-health` 的對應 scenario 一個測試，時間全部由假 `Clock` 提供

## 2. 公開行情

- [ ] 2.1 Binance：`premiumIndex`、`fundingInfo`（週期）、`exchangeInfo`（上市過濾與 `LOT_SIZE` / `MARKET_LOT_SIZE`）、24h tickers → `FundingObservation` 與 `InstrumentRules`；週期一致性檢查。先紅後綠；fixtures 由 `curl` 錄製存入 `app/tests/fixtures/binance/`（檔頭記錄錄製時間與指令）。驗收：`exchange::binance` 測試通過，案例含 `SETTLING` 排除、週期缺失為 `DATA_ERROR`、週期過舊為 `DATA_ERROR`
- [ ] 2.2 Bybit：`tickers`、`instruments-info`（`fundingInterval` 分鐘、`LinearFutures` 排除、`fundingIntervalHour` 交叉檢查）、分頁取完、頁數上限與重複 cursor 中止、`Incomplete`。先紅後綠；fixtures 含「第一頁 500 筆帶 cursor」「第二頁失敗」「cursor 重複」。驗收：`exchange::bybit` 測試通過
- [ ] 2.3 OKX：`funding-rate`（`fundingTime` 作為下次結算、週期由時間差推導、`method` 檢查）、`instruments`（`ctVal`、`lotSz`、`minSz`、上市過濾）、`tickers` 的 `volCcy24h × last` 換算。先紅後綠。驗收：`exchange::okx` 測試通過，案例含 `volCcy24h` 缺 `last`
- [ ] 2.4 單一標的重新抓取（三所）與逾時分級：以假傳輸層證明每次呼叫都發出新請求、不讀快取、`observed_at` 取多個請求中最早者。先紅後綠。驗收：`exchange::refetch` 測試通過；並以真實公開端點各打一次，貼出指令與耗時（對照 design 的延遲觀察）
- [ ] 2.5 Binance WebSocket `!markPrice@arr@1s`：解析、部分更新、缺欄位略過、來源健康狀態（斷線即時生效、靜默連線重連、指數退避）、快取讀取介面強制附帶狀態與年齡、狀態轉換才寫事件（`FETCH_ERROR` / `FEED_RECOVERED`）。先紅後綠。驗收：`exchange::feed` 測試通過；再對真實串流連線 25 秒，貼出各標的更新間隔的 p50 與 p95

## 3. 簽名 GET

- [ ] 3.1 Binance：簽名（使用校時後時間、`recvWindow` 5000）、餘額、持倉（過濾零數量、Decimal）、未成交委託；金鑰經 `secret-storage` 取得，取不到即 `NotConnected` 且不發請求；錯誤遮蔽。先紅後綠，以假傳輸層驗證目的主機與零請求。驗收：`exchange::binance_signed` 測試通過
- [ ] 3.2 Bybit：簽名、餘額、持倉與未成交委託（分頁取完、`Incomplete`）、`retCode` 檢查；OKX 帳戶方法回傳「不支援」。先紅後綠。驗收：`exchange::bybit_signed` 與 OKX 不支援的測試通過

## 4. 驗證

- [ ] 4.1 故障注入整合測試：分頁截斷、429（含與不含 `Retry-After`）、逾時、WebSocket 斷線與靜默、時鐘偏移、時間戳被拒絕。驗收：`cargo test -p tong-funding exchange::` 全數通過，貼出通過數量與指令；另以 `cargo test -p tong-funding-core` 證明 `core` 未被改動
- [ ] 4.2 對真實 demo 帳戶讀取一次餘額、持倉與未成交委託（Binance 兩個候選主機各試一次以決定常數；Bybit 一次），貼出指令與已遮蔽的輸出；同時記錄 design.md Open Questions #2–#5 的實測結果，並以公開端點重驗週期涵蓋率（Binance `TRADING` 永續全數有週期）與 OKX 成交量換算。驗收：design.md 更新「未驗證」清單；未能取得 demo 金鑰時，此 task 標示未完成，本 change 不得封存
