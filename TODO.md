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
- [ ] **demo/testnet 簽名 GET 實測**（exchange-readonly-adapters task 4.2）：需要使用者的 Binance 與 Bybit demo 金鑰；
      須決定 Binance 簽名該用 `testnet.binancefuture.com` 還是 `demo-fapi.binance.com`（design.md Open Questions）。
      沒有金鑰時該 change 不得封存。
