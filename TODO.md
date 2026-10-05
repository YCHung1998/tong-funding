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
