# TODO（未來可追蹤；跨專案任務板見 /Users/eason.hung/Documents/github/TASKS.md）

## 效能量測（pending，使用者 2026-10-05 決定先擱置）
- [ ] 在「接電源、關閉低電量模式、桌面無其他視窗遮擋」下重跑表格基準，並把結果補進
      `openspec/changes/archive/2026-10-05-bootstrap-gpui-shell/design.md` 的量測紀錄表（增加「電源狀態」欄）：
      `cargo build --release -p tong-funding && ./target/release/tong-funding --bench-table 1|2|10 10`
      輸出含 `dropped=`（掉幀）與 `paused=`（視窗被暫停，不計入）。預算：掉幀率 ≤ 0.5%（以當下 p50 為基準）且每幀 CPU < 一個 vsync 週期。
- 背景：2026-10-05 量測時機器為電池 + 低電量模式（`pmset -g` 顯示 `lowpowermode 1`），更新率被降到約 30Hz，數據不可靠；
  較早一組 60Hz 的量測（p95 約 17.2–17.7 ms、無掉幀、每幀 CPU 約 5 ms）狀態未記錄。
- 真實資料與真實更新頻率的重量測另見 `ui-readonly-pages` task 4.2。

## 需要使用者在場的驗證（agent 不得代做，因為會動到真實 Keychain 或需要 demo 金鑰）
- [ ] **真實 macOS Keychain**（store-sqlite task 4.1）：執行
      `cargo test -p tong-funding store::secrets::tests::real_keychain -- --ignored --nocapture`
      （用獨立 service 名稱 `tong-funding-test`，不會碰真正的 `tong-funding` 金鑰；會跳 macOS 授權視窗）。
      把「是否每次都跳授權提示、開發版 binary 的行為」記進 store-sqlite 的 design.md（change 封存前）。
- [ ] **舊 `events.jsonl` 複本實跑**（store-sqlite task 5.2）：先複製檔案，再對複本與一個暫存資料庫執行
      `cp mvp-python/data/events.jsonl /tmp/events-copy.jsonl && shasum -a 256 /tmp/events-copy.jsonl`
      `cargo run -p tong-funding -- import-legacy-events /tmp/events-copy.jsonl --db /tmp/funding-import-test.db`
      （連跑兩次，第二次應為 `imported: 0`）。把報告與前後 SHA-256 貼進 store-sqlite 的 design.md。
- [ ] **把 demo 金鑰放進 Keychain**（取代 Python 版的 `.env`；**不要**放進 repo、GitHub 或對話）：在 Mac 上用既有的 `.env` 一次匯入（值不會顯示）
      `cargo run -q -p tong-funding -- secrets import-env /path/to/mvp-python/.env`
      `cargo run -q -p tong-funding -- secrets status` 確認每項為 `present`；之後可以刪除 `.env`。
      （單項更新：`pbpaste | cargo run -q -p tong-funding -- secrets set binance api-key`）
      註：雲端開發環境的網路連不到交易所，真實 demo 驗證都要在 Mac 上跑。
- [ ] **demo/testnet 簽名 GET 實測**（exchange-readonly-adapters task 4.2）：需要使用者的 Binance 與 Bybit demo 金鑰；
      須決定 Binance 簽名該用 `testnet.binancefuture.com` 還是 `demo-fapi.binance.com`（design.md Open Questions）。
      沒有金鑰時該 change 不得封存。

## exchange-demo-execution：需要使用者在場（Mac + demo 金鑰；agent 不得代做）
真實下單程式（`app/src/exchange/execution/`）已以錄製回應測完，但**從未連過交易所**；引擎尚未接到 `main.rs`，所以實測先用一個 `#[ignore]` 的探測測試（只對寫死的 demo/testnet 主機、需環境變數確認才會下單）。
- [ ] **0. 金鑰進 Keychain**（若尚未做）：`cargo run -q -p tong-funding -- secrets import-env /path/to/mvp-python/.env`，再 `cargo run -q -p tong-funding -- secrets status` 確認 Binance、Bybit 的 `api-key` / `api-secret` 皆為 `present`。
- [ ] **1. 帳戶設定**：兩個 demo 帳戶都設為**單向持倉（one-way）**，選一個兩所都能小量交易的標的（建議 `ETHUSDT`），數量取兩所最小下單量中較大者且名目達 Binance 最小名目（ETH 約 `0.01`，以交易所頁面為準）。
- [ ] **2. 單輪探測**（約 1 分鐘，會在兩所各開、平一次小量倉；第一次讀 Keychain 會跳授權視窗）：
      `TONG_DEMO_LIVE=I_AM_PRESENT_PLACE_DEMO_ORDERS TONG_DEMO_SYMBOL=ETHUSDT TONG_DEMO_QTY=0.01 TONG_DEMO_ROUNDS=1 TONG_BINANCE_HOST=testnet cargo test -p tong-funding --release exchange::execution::live_probe::live_demo_probe -- --ignored --nocapture`
      若 Binance 回金鑰 / 簽名錯誤（如 `-2014`、`-2015`），改 `TONG_BINANCE_HOST=demo` 重跑。
      要記下（貼回對話，或寫進 `openspec/changes/exchange-demo-execution/design.md` 的未驗證項）：
      (a) 哪個 Binance 主機接受金鑰（Open Question 1）；(b) 兩所是否接受引擎格式的 `client_order_id`（輸出中 `accepted` 且回報同一 id）；
      (c) 成交、手續費與幣別是否出現（`fills:` 行；Bybit 是否需要退到 history）；(d) 平倉後兩所持倉是否為 0、委託是否為空（`after close` 行）；
      (e) 最後兩行「reduce-only without a position」各所的拒絕錯誤碼與訊息；(f) 任何 `position mode` 拒絕（代表帳戶不是單向）。
      事件存在 `/tmp/tong-demo-probe.db`（`TONG_DEMO_DB` 可改）。
- [ ] **3. T−5 決策的延遲量測**（engine-simulation D9：「進場觸發 → 兩腿皆被接受」p99 < 2,500 ms 才改 T−5）：同上指令改 `TONG_DEMO_ROUNDS=20`（40 張開倉單 + 40 張平倉單，約 3–5 分鐘），貼出最後的 `latency summary` 四行。
      注意：探測量的是「送單 → 兩腿 ACK」（含快取失效時的一次持倉模式查詢），**不含**引擎進場前的單一標的重抓（已量 0.16–0.26 秒）；判斷時以 `open trigger -> both legs accepted` 的 p99 加約 300 ms 估計，若接近 2,500 ms 請保持 T−10。
      逐筆資料：`sqlite3 /tmp/tong-demo-probe.db "SELECT json_extract(payload,'$.exchange'), json_extract(payload,'$.action'), json_extract(payload,'$.result'), json_extract(payload,'$.latency_ms') FROM events WHERE event_type='ORDER_LATENCY' ORDER BY id"`
      引擎接上 `main.rs` 後，正式數字改用 app 資料庫內引擎寫的 `ORDER_LATENCY`（其 `triggered_at` 為進場觸發時間，`engine::latency::LatencyReport` 計算）。
- [ ] **4. macOS 通知機制（task 3.1 未完成部分）**：在 Mac 終端執行 `osascript -e 'display notification "tong-funding test" with title "tong-funding"'`，回報通知是否出現（含專注模式 / 通知權限的情況，以及是否顯示為「Script Editor」或終端機發出）。依結果再實作真正的 `Notifier`（目前只有寫 stderr 的 `LogNotifier`）並貼一次實際出現的截圖。
- [ ] **5. 4.2 的其餘部分需要引擎接進 app 後才能做**：在 EXCHANGE_DEMO 下由引擎自己完成一輪小量開平倉，並在兩個時點（送單後回應前、兩腿成交後）終止程式重啟，確認對帳結果與交易所端持倉 / 委託一致。接線本身是另一個 task（`main.rs` 與 UI 不屬於本 change）。
- [ ] **6. 4.3 警示頻率**（運行期間結束後；app 資料庫在 `~/Library/Application Support/tong-funding/funding.db`）：
      `sqlite3 ~/Library/Application\ Support/tong-funding/funding.db "SELECT json_extract(payload,'$.reason') AS reason, COUNT(*) FROM events WHERE event_type='PAIR_ALERT' AND ts_ms BETWEEN <開始ms> AND <結束ms> GROUP BY reason"`
      連同期間長度與 Python 版 47 筆（其中 46 筆疑似測試汙染）的對照，由使用者判斷完全人工的負擔是否可接受。

## ui-readonly-pages：需要在 Mac 上做的驗證（task 4.1、4.2；agent 無顯示器、連不到交易所）
在 `/Users/eason.hung/Documents/github/tong-funding`、分支 `feat/ui-readonly-pages` 上操作。前置：上面「把 demo 金鑰放進 Keychain」已完成（`secrets status` 全部 `present`）。

- [ ] **4.1 真實 demo 帳戶逐頁截圖並對照 Figma**
  1. **先關閉 App**（資料庫是單一實例，App 開著時 `config` 會被拒絕），輸入費率與門檻（數值請依你的 demo 帳戶手續費等級；以下只是範例，`risk` 會整個取代）：
     `cargo run -q -p tong-funding -- config set risk '{"net_edge_threshold_pct":"0.01","est_slippage_pct":"0.02","taker_fee_pct":{"Binance":"0.05","Bybit":"0.055","Okx":"0.05"}}'`
     `cargo run -q -p tong-funding -- config show` → 最後一行要是 `required fields: complete`。
     （選用的每所覆寫：`config set risk_overrides '{"Bybit":{"est_slippage_pct":"0.03"}}'`；寫錯欄位會被拒絕並指出欄位名。）
  2. 啟動：`cargo run --release -p tong-funding`。Binance 簽名預設打 `testnet.binancefuture.com`；若 Binance 卡片顯示授權失敗，關掉後改用
     `TONG_FUNDING_BINANCE_HOST=demo cargo run --release -p tong-funding` 再試，並記下哪個主機成功。
  3. 等約 40 秒（校時 → 帳戶輪詢、行情每 10 秒一輪），再截圖（`⌘⇧4` 再按空白鍵截整個視窗），存到 `openspec/changes/ui-readonly-pages/screenshots/`：
     - `dashboard.png`：總覽（總資產、兩所卡片與兩張甜甜圈、資產明細、OKX 說明卡、曝險摘要、右上「帳戶刷新 Ns」）。
     - `positions.png`：持倉（篩選器、三張彙總卡、持倉表含「未配對」與「Funding 收到 —」、提示列）；再取消勾選一個交易所截 `positions-filtered.png`。
     - `system-log.png`：系統日誌（筆數與時間範圍、類型篩選、SCAN_RUN「僅本次運行」、匯入事件的「匯入」標示）；按「載入更早事件」截 `system-log-older.png`。
     - `scanner.png`：掃幣（頁首門檻唯讀與連結、三個來源狀態、最新掃描時間、彙總卡、矩陣含週期標籤與倒數）；開啟「只顯示達標」截 `scanner-qualified.png`；按「立即刷新」時截 `scanner-refreshing.png`（按鈕顯示「刷新中…」）。
     - **橫幅**：關掉 Wi-Fi 約 40 秒，等出現「Bybit 行情已 N 秒未更新」或斷線警示後，依序點 8 個側欄頁面各截一張 `banner-<頁名>.png`（證明橫幅常駐）；打開 Wi-Fi 後到系統日誌確認有一筆 `FETCH_ERROR` 與較晚的一筆 `FEED_RECOVERED`，截 `system-log-recovered.png`。
  4. 與 Figma 並排對照，把每一處差異補進 `openspec/changes/ui-readonly-pages/design.md` 的「Figma 差異對照表」，並更新「未驗證項目清單」：
     - **資產列對應**：Binance / Bybit 各所 Value 與交易所網頁上的總額比對（截交易所畫面），決定是否需要「合約」列（design.md 實作紀錄 #2：目前沒有合約列，以免重複計入已用保證金）。
     - **initial margin**：保證金分布圖是否出現「估算」（出現代表交易所沒回報該欄位，Binance 全倉時預期如此）。
     - 持倉在引擎出現前全部「未配對」是預期（design D12、Open Question 7），請決定是否接受以此狀態通過 4.1。
  5. 回報：截圖、各所總額與交易所畫面的差額、哪個 Binance 主機成功、design.md 實作紀錄 1–14 中不同意的項目。

- [ ] **4.2 528 列掃幣表的幀時間（真實更新頻率）**
  1. 接電源、關閉低電量模式（`pmset -g | grep lowpowermode` 應為 `0`），關掉其他會遮住視窗的程式。
  2. `cargo build --release -p tong-funding`
  3. `TONG_FUNDING_FRAME_STATS=60 ./target/release/tong-funding`，切到「掃幣」頁並保持視窗可見（量測期間每幀都重繪，是最壞情況；Binance WebSocket 每秒、Bybit/OKX 每 10 秒更新）。
     每 60 秒 stdout 會印一行 `FRAMES page=scanner rows=… frames=… p50_ms=… p95_ms=… max_ms=… dropped=… paused=…`；收集至少 3 行，另外各捲動表格一次、開關「只顯示達標」一次。
  4. 把結果（含 `rows=` 實際列數、電源狀態）填進 `openspec/changes/archive/2026-10-05-bootstrap-gpui-shell/design.md` 的量測紀錄表；預算 p95 ≤ 16.7 ms（或該 change 放寬後的值）。未達標就照該 change 的緩解順序處理並回報。

## ui-trading-pages：需要在 Mac 上做的驗證（task 5.1；agent 無顯示器、連不到交易所）
在 `/Users/eason.hung/Documents/github/tong-funding`、分支 `feat/ui-trading-pages` 上操作。前置：demo 金鑰已在 Keychain（`cargo run -q -p tong-funding -- secrets status` 全部 `present`），且已依上節 4.1 設好費率與門檻（`config show` 最後一行 `required fields: complete`）。App 開著時不要跑 `config set`（單一資料庫實例）。截圖存到 `openspec/changes/ui-trading-pages/screenshots/`。

- [ ] **5.1-a 啟動與組裝根**
  1. `cargo run --release -p tong-funding`（Binance 主機若授權失敗改 `TONG_FUNDING_BINANCE_HOST=demo`）。
  2. 等約 40 秒：狀態列徽章顯示 `SIMULATION`；系統日誌應出現行情 `SCAN_RUN`，且**沒有** `MODE_LOAD_WARNING` 以外的錯誤。若之前存過 `EXCHANGE_DEMO` 而金鑰失效，頂部應有 `EXECUTION_MODE_FALLBACK` 橫幅。
  3. 終端機沒有任何金鑰字樣（`grep -i secret` 應無輸出）。
- [ ] **5.1-b 四頁截圖並與 Figma 對照**（各一張，`staged-orders.png`、`contract-settings.png`、`risk-settings.png`、`manual-order.png`，再加 `scanner-candidates.png`）
  - 合約設定：輸入 1200 / 3 → 顯示「1,200 ÷ 3 = 400 / leg」、雙腿 2,400 / 800；切「用保證金反推槓桿」輸入 400 → 3×；試算 BTCUSDT 三所數量（Binance 若在 1 秒內更新應有數量；**Bybit / OKX 每 10 秒輪詢，預設 `stale_data_threshold_ms` 1000，預期顯示「價格已過期」**——回報實際情況，決定是否接受，見 design.md 實作紀錄 #7）。按「儲存模板」，系統日誌出現 `CONTRACT_SETTINGS_UPDATED`（含 before / after）。
  - 風控設定：確認沒有 Funding Threshold / Max Concurrent Trades / Hedge Threshold、有「最大價格漂移」與「估計滑價」兩欄、`Min Expected Net PnL %` 預設 0.03 與 Net Edge 門檻並存；把 `max_leverage` 改 0 → 欄位錯誤且儲存禁用；改回並儲存 → `RISK_CONFIG_UPDATED`（含前後值）。Bybit 開 `max_leverage` 覆寫 = 4，看預覽 Binance×Bybit 為 4。
  - 逐項列出與 Figma 的刻意差異，核對 design.md 差異表。
- [ ] **5.1-c SIMULATION 一輪「掃幣 → 交易單 → 持倉」**
  1. 掃幣頁勾一個「達標」列的「加入交易單」→ Candidate List 出現（每腿 Notional / 槓桿 / Margin 等於模板）→「加入並前往交易單 →」。系統日誌出現 `PAIR_PREPARED`，沒有任何 `ORDER_SUBMITTED`。
  2. 交易單頁：該列數量為取整後的值；把 `trigger_mode` 切成 MANUAL（日誌 `TRIGGER_MODE_CHANGED`）；勾選 →「一鍵送出已選取」→ 確認面板列出 2 腿與「SIMULATION：由模擬器成交，不會送出真實訂單」→ 先按取消（日誌無變化）→ 再送出並確認。
  3. 預期：`PRE_TRADE_CHECK` → `ORDER_SUBMIT` → `RECONCILED`（或 BLOCKED 並在「上次執行結果」列出全部未通過檢查名稱）；`ORDER_SUBMITTED` 的 `simulated` 為 true。RECONCILED 後在 MANUAL 下按「立即平倉」→ 確認 → `FINALIZED`。截圖 `sim-round-*.png`。
  4. AUTO 防重複：再加一個配對、`trigger_mode` 切 AUTO，在進場時點（T−10 秒）前後各按一次一鍵送出：只應有一次 `PRE_TRADE_CHECK` 與兩張開倉單；第二次的回覆顯示「not PREPARED」。
- [ ] **5.1-d EXCHANGE_DEMO 一輪與單腿失敗的人工處理**
  1. 風控頁選 `EXCHANGE_DEMO` → 確認面板 → 確認；徽章變 `EXCHANGE_DEMO`，日誌 `EXECUTION_MODE_CHANGED`。（設定不完整或金鑰缺時選項應禁用並說明原因，各截一張。）
  2. 用最小數量重複 5.1-c 的一輪；`ORDER_SUBMITTED` 的 `simulated` 為 false，「上次執行結果」顯示交易所 order id；持倉頁看到兩腿；平倉後兩所帳戶皆為 0。
  3. 單腿失敗：在 Bybit demo 先把可用保證金降到不足（或把該標的槓桿設到交易所會拒絕的值），再送一輪 → 配對應為 `PARTIAL_FAILURE`，交易單頁「需人工處理」區塊顯示兩腿持倉；「人工確認已平倉」在仍有持倉時禁用並寫「Binance 仍有持倉」；按「人工要求平倉」→ 確認 → 平掉剩下一腿 → 約 15 秒後（帳戶讀取週期）「人工確認已平倉」可按 → 配對 `FINALIZED`。截圖 `partial-failure-*.png`，並回報入口是否夠用（design.md Risks：Python 版 47 筆單腿失敗）。
- [ ] **5.1-e 手動下單頁**：SIMULATION 下送 Binance BTCUSDT BUY 0.0014 → 確認面板顯示 0.001，結果列「[模擬] 下單成功」；切 EXCHANGE_DEMO 送同一筆 → 交易所 order id；用結果中的 client_order_id 撤單（已成交時應顯示「訂單已是 Filled，未撤銷」而不是成功）；開 kill switch 後非 reduce-only 的 Submit 禁用並顯示「緊急停止中」，勾 reduce_only 可送。
- [ ] **5.1-f rate limit**：運行 30 分鐘後到系統日誌確認沒有 `RATE_LIMITED`（交易頁每 15 秒讀一次兩所持倉 / 委託 / 保證金，另有 30 秒的帳戶輪詢）；若有，回報次數。

## funding-pnl：需要在 Mac 上做的驗證（task 1.1、4.1 截圖、5.1；agent 連不到交易所、沒有金鑰）
在分支 `feat/funding-pnl` 上操作。前置：上面「把 demo 金鑰放進 Keychain」已完成（`cargo run -q -p tong-funding -- secrets import-env /path/to/mvp-python/.env`，再 `secrets status` 全部 `present`）。
目前的流水解析器與 fixtures 只依公開文件撰寫（fixtures 標頭寫著 `UNVERIFIED, FROM DOCS`），這三項做完前不得封存本 change。

- [ ] **1.1 先驗證再信任解析器**（唯讀簽名 GET，各打一次；不下單）
  1. 先確認 demo 帳戶過去 7 天內有持倉跨過 funding 結算（沒有的話先完成 5.1 的一組配對再回來做）。
  2. 用 `curl` 或一次性的 `cargo test -- --ignored` 都可以；最簡單是 Python（金鑰從 Keychain 讀，不要貼進終端歷史）：
     - Binance：`GET https://testnet.binancefuture.com/fapi/v1/income?symbol=<SYMBOL>&incomeType=FUNDING_FEE&startTime=<7 天前 ms>&endTime=<現在 ms>&limit=1000&timestamp=…&recvWindow=5000&signature=…`（header `X-MBX-APIKEY`）；若 testnet 拒絕，改 `demo-fapi.binance.com`，記下哪個主機成功。
     - Bybit：`GET https://api-demo.bybit.com/v5/account/transaction-log?accountType=UNIFIED&category=linear&currency=USDT&type=SETTLEMENT&startTime=…&endTime=…&limit=50`（header `X-BAPI-API-KEY`、`X-BAPI-SIGN`、`X-BAPI-TIMESTAMP`、`X-BAPI-RECV-WINDOW`）。
  3. 把回應去敏（刪掉帳戶 id、`orderId`、`tradeId` 等可識別欄位；保留欄位名稱、型別、正負號、時間）存成
     `app/tests/fixtures/funding/binance_income_funding_fee.json` 與 `bybit_transaction_log_settlement.json`（保留 `{"_fixture_note", "request", "response"}` 結構，`_fixture_note` 改成「RECORDED <日期> from demo, de-identified」）。
     注意：測試目前檢查 `_fixture_note` 以 `UNVERIFIED, FROM DOCS` 開頭，換成真實回應時一併改 `app/src/exchange/signed/ledger.rs` 測試裡的那行斷言與期望值。
  4. 逐項填 `openspec/changes/funding-pnl/design.md` 的「驗證紀錄」表：欄位名稱、`income`／`funding` 正負號（收到是正嗎？對照交易所網頁的資金費紀錄）、`time`／`transactionTime` 單位與是否等於結算時刻（差幾秒？影響時間軸 ±60 秒容差）、`tranId`／`id` 是否唯一、單頁上限、`nextPageCursor` 行為、funding rate 為 0 的結算是否仍有流水、成交手續費幣別。
  5. 跑 `cargo test -p tong-funding funding_parse`；與文件不符處修正解析器、spec 與 design。

- [ ] **4.1 持倉頁截圖對照 Figma**（view-model 與畫面已完成，只差截圖）
  1. App 目前沒有啟動引擎（見 design 實作紀錄 #17），持倉頁的 Funding 欄在沒有配對時都顯示「—（尚未取得）」或「—」（未配對）是預期。
  2. 啟動 `cargo run --release -p tong-funding`，持倉頁截 `openspec/changes/funding-pnl/screenshots/positions-funding.png`；與 Figma 持倉頁並排，把差異（Funding 收到欄、「價差，未含 funding 與手續費」註記、配對卡的已付開倉手續費／進行中合計／結算時間軸／預期對實際面板）列進 design.md。
  3. 若要看到有資料的畫面，等引擎接上 App 後在 demo 跑一組配對（5.1）再截一次。

- [ ] **5.1 demo 帳戶走完一組配對跨過至少一次結算**（需要引擎接上 App，或 exchange-demo-execution 的實測工具）
  1. 在結算前約 1 分鐘進場、結算後平倉（EXCHANGE_DEMO）。
  2. 平倉確認後，確認系統日誌依序出現：`CLOSE_CONFIRMED` →（流水未齊時）`PNL_PENDING` → `FUNDING_LEDGER_FETCHED`（兩所各一筆以上）→ `PAIR_PNL_COMPUTED` → `FINALIZED`；最多等 10 分鐘（`PNL_RETRY_WINDOW_MS`，暫定）。
  3. 記下：結算到流水出現的實際延遲（校正 `FETCH_DELAY_MS`／`FETCH_RETRY_MS`／`PNL_RETRY_WINDOW_MS`）、`PAIR_PNL_COMPUTED` 的狀態與原因（平倉參考價已在送平倉單前記錄，design 實作紀錄 #6；兩所流水齊全、手續費有回報時應為 COMPLETE；若出現「無參考價」，查平倉 `ORDER_SUBMITTED` 的 `reference_error`）、各分量數字、對帳 `PNL_RECONCILIATION` 結果、預期對實際的差異。
  4. 再跑一次同一窗的取得，確認 `FUNDING_LEDGER_ENTRY` 筆數不變（去重）。
  5. 把實際數字填進 design.md 的「驗證紀錄」最後一行，回報指令與測試檔路徑。
