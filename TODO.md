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
