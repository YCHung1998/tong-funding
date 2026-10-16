# Live（真錢）操作手冊：從金鑰輸入到第一筆實單

> 狀態（2026-10-16）：程式與測試已完成並整合（app 1959 個測試通過、0 失敗），**但沒有任何一條 live 路徑對真實交易所跑過**，也沒有人眼看過畫面。所以從階段 0（只讀）開始，不要直接下單。規劃見 `docs/real-trading-plan.md`。

## 0. 先決條件（在交易所端，你自己做）
1. 兩所各開一個**專用子帳戶**，各存 30 USDT（最小名目不夠時可加碼到 50）。
2. 子帳戶設定：單向持倉；全倉（Bybit 必須是預設的 `REGULAR_MARGIN`，不要逐倉或組合保證金）；Binance 不開 Multi-Assets 與 Portfolio Margin。
3. 為子帳戶建立 API 金鑰：**只開合約交易、不開提領、設 IP 白名單**（你目前對外 IP）。
4. 詳細步驟見 `docs/real-trading-plan.md` 第 9 節。

## 1. 把金鑰放進 live 的鑰匙圈（只有你輸入，不要貼給任何人）
在專案根目錄（用你自己的終端機，值從標準輸入讀，不會出現在指令列或歷史紀錄）：

```
cargo run -p tong-funding -- secrets set --live binance api-key
cargo run -p tong-funding -- secrets set --live binance api-secret
cargo run -p tong-funding -- secrets set --live bybit api-key
cargo run -p tong-funding -- secrets set --live bybit api-secret
cargo run -p tong-funding -- secrets status --live
```

- 每個指令執行後輸入該值再按 Enter。
- `secrets status --live` 只顯示「已設定／未設定」，不顯示值。
- live 金鑰存在獨立的鑰匙圈項目 `tong-funding-live`；demo 金鑰項目不受影響；`secrets import-env` 不能寫 live（刻意設計）。
- 不要把 live 金鑰放進 `.env`、不要貼進對話。

## 2. 啟動 live 程式
```
cargo run -p tong-funding -- --live
```
- 視窗標題會是 `[LIVE]`，每一頁頂端有紅色橫幅「LIVE 真錢」，並顯示「交易已鎖定（只能平倉）」。
- 同一時間只能開一個程式（demo 與 live 不能同時開）。
- live 使用獨立的資料庫 `funding-live.db`（與 demo 的 `funding.db` 分開）。
- 每次啟動 live 都是**鎖定**狀態（kill switch ON），不會自動武裝。

## 3. 設定上限（風控設定頁）
live 資料庫初始值：觸發模式 `MANUAL`、提早平倉 0、合約模板名目 9 USDT；兩個 live 必填上限沒有預設值，要在畫面填：
- `live_max_leg_notional_usdt`：階段 2 建議 **10**（程式內建天花板 100）。
- `live_max_pairs_per_arming`：階段 2 建議 **2**（天花板 5）。
- 槓桿 5、同時配對數 1。
- **名目要設在上限的 99% 以下**：進場前的保護價有 1% 緩衝，名目恰好等於上限會被送單前檢查拒絕（所以模板預設 9 而不是 10）。
- 命令列 `config set` 不能寫這些受保護欄位（刻意設計）。

## 4. 階段 0：只讀（不下單）
1. 啟動 live、保持鎖定。
2. 在風控設定頁的武裝面板按「執行預檢」，看每項通過／失敗與原因：金鑰、兩所唯讀餘額與持倉、校時、單向持倉、帳戶無既有部位、保證金、上限、資料庫可寫與磁碟空間。
3. 失敗的項目照原因處理（例如 IP 白名單不符會讓唯讀讀取失敗）。**Bybit 在沒有持倉時讀持倉模式的行為沒有實測**，若預檢因此失敗請把原因文字（不含金鑰）告訴我。
4. 目標：預檢全綠，且鎖定時不會有任何下單。

## 5. 階段 2：極小額、人工、第一次真錢
1. 預檢全綠後，勾選人工清單（提領已關、IP 白名單、專用子帳戶只存小額、已在交易所核對手續費率）。
2. 輸入確認字串：`我了解這是真錢 LIVE`，按「武裝」。武裝後橫幅變為「交易已武裝」。
3. 一次武裝最多進場 2 筆配對，之後自動鎖定；再交易必須重新預檢、重新武裝。
4. 手動下單頁在 live 只能平倉與取消；一腿成交另一腿失敗時，用配對卡上的「平掉裸露的一腿」，或按「前往交易所」到網頁介面手動平倉。
5. 出現任何 `PARTIAL_FAILURE`／`IMBALANCED`／`UNRESOLVED`、認證錯誤、HTTP 418／429、校時失敗時，系統**自動鎖定**（只擋開倉，平倉照常）。
6. 隨時可按「立即鎖定」。
7. 每筆結束後用 `python3 issue/tools/trade_table.py`（指向 live 資料庫時請設定 `TONG_FUNDING_DB=~/Library/Application\ Support/tong-funding/funding-live.db`）整理損益，對照交易所帳戶餘額。

## 6. 已知缺口（還沒做，AUTO 之前必須補）
- 資料庫寫入失敗時的「只平倉緊急通道」（目前只有停機警示，需到交易所手動平倉）。
- 每日損失上限與自動對帳（目前靠你每天手動核對）。
- 手續費率與金鑰權限的 API 預檢（目前是人工勾選）。
- macOS 系統通知；鎖取不到時的訊息只在終端機。
- 認證錯誤的判定清單未經實機驗證（可能誤鎖，寧可鎖）。
- AUTO 觸發模式在 live 被禁止。

## 7. 回滾
- 不想再用 live：直接用不帶 `--live` 的 `tong-funding`，它只會碰 demo／模擬，且傳輸層拒絕 live 主機。
- 緊急情況：先按「立即鎖定」，再到交易所網頁平倉；必要時在交易所停用該 API 金鑰。
