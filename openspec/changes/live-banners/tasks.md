## 1. 單一來源

- [ ] 1.1 盤點所有顯示環境的位置（`rg "EXCHANGE_DEMO|SIMULATION|simulated"` 於 `app/src/ui`、`app/src/engine/alert.rs`），列成清單寫進 design 的附註；新增 `ui/vm/env_label.rs`（`EnvLabel`）：輸入 `(ProcessIdentity, ExecutionMode)`，輸出橫幅文字、色調、標題前綴、徽章文字、確認按鈕文字；紅色測試先行：2×2 組合的文字內容（Live 含「LIVE」「真錢」，Demo／模擬不含「LIVE」）、窮舉 match

## 2. 橫幅與標題

- [ ] 2.1 在 `Shell` 頁面框架最上層加入橫幅（所有頁面共用、不可關閉），內容與 kill switch 現況來自 vm；vm 測試：沒有隱藏狀態欄位、文字存在、Live 與 Demo 的內容不同
- [ ] 2.2 視窗標題依身分加 `[LIVE]`／`[DEMO]`；測試：標題字串

## 3. 確認與文案

- [ ] 3.1 把「加入交易單」「進場」「平倉」「立即平倉」與模式切換的確認文字函式化，加入 `EnvLabel` 與金額；Live 按鈕文字「以真錢下單」／「以真錢平倉」；vm 測試涵蓋 Live 與 Demo
- [ ] 3.2 `engine/alert.rs` 的通知文字改由傳入的 `EnvName` 產生；測試：Live 的 `PARTIAL_FAILURE` 通知標示 LIVE（紅色先行：現況會寫 DEMO）
- [ ] 3.3 `ui/vm/manual_order.rs` 帳本來源與 `trading_pages.rs` 的執行模式說明、配對卡徽章、按鈕啟用條件改為窮舉；測試：Live 手動下單頁標示 LIVE 帳戶
- [ ] 3.4 Live 身分下配對卡、交易單列、訂單與系統日誌列顯示 `LIVE` 徽章；vm 測試

## 4. 規格與驗證

- [ ] 4.1 套用 `app-shell` 規格修改（已在本變更的 delta 規格）；新增測試掃描 `ui/` 的非測試程式碼，不得再有硬寫的環境字串（只允許出現在 `env_label.rs`）
- [ ] 4.2 `openspec validate live-banners` 通過；`cargo test -p tong-funding-core -p tong-funding` 全綠並記錄指令與通過數量；GPUI 煙霧測試（若框架可用）；用乾淨的編譯目錄執行
