## Why

`live-process` 讓程式能以 `--live` 身分對 Binance／Bybit 正式環境下真單。使用者最在意的是**絕對不能用錯環境**：看一眼就知道現在是真錢、demo 還是模擬，而且每一個會花錢的確認動作都再說一次。

現況有兩個問題：(1) `app-shell` 規格明文規定畫面「不出現 LIVE 字樣」，而且畫面只有 Demo／Testnet 標示；(2) 好幾處顯示環境的文字把「非模擬」一律寫成 `EXCHANGE_DEMO`（例如 `app/src/engine/alert.rs:143` 的系統通知、`app/src/ui/vm/manual_order.rs:396-398` 手動下單頁的帳本來源、`app/src/ui/trading_pages.rs` 多處說明文字），在 live 身分下會顯示成 demo，造成誤導。

## What Changes

- **每一頁頂端固定一條不可關閉的橫幅**，由程式身分產生：Live 為紅色「⚠ LIVE 真錢 · Binance + Bybit 正式環境」、Demo 為琥珀色「DEMO（模擬帳戶，不涉及真錢）」、模擬模式為灰色「模擬」。一律用文字標明環境，不只靠顏色。
- **視窗標題**帶環境：Live 為 `[LIVE]`，Demo 為 `[DEMO]`。
- **橫幅同時顯示 kill switch 狀態**：Live 下為「交易已鎖定（只能平倉）」或「交易已武裝」。
- **確認對話框**：所有「加入交易單」「進場」「平倉」「立即平倉」類的確認，重複顯示環境與金額；Live 的確認按鈕文字改為「以真錢下單」。
- **文案由身分產生，不由布林產生**：系統通知（`engine/alert.rs`）、手動下單頁帳本來源標示、執行模式說明與按鈕啟用條件，全部改成依身分與模式窮舉產生，不再把非模擬一律寫成 `EXCHANGE_DEMO`。
- **修改 `app-shell` 規格**：「不得出現 LIVE 字樣」改為「Live 程式必須顯示 LIVE；Demo 與模擬程式不得出現 LIVE」。
- **Live 身分下每個配對卡、交易單列、訂單與系統日誌列顯示環境徽章**（`LIVE`），讓截圖與紀錄也一眼可辨。

## Capabilities

### New Capabilities

- `live-banners`：依程式身分的環境橫幅、視窗標題、確認對話框文字、環境徽章與文案產生規則。

### Modified Capabilities

- `app-shell`：把「不出現 LIVE 字樣」改為依身分的顯示規則，並更新相關情境。

## Impact

- 程式：`app/src/ui/shell.rs`（橫幅與標題）、`app/src/ui/status.rs`（環境徽章）、`app/src/ui/trading_pages.rs`（確認文字與模式說明）、`app/src/ui/vm/manual_order.rs`、`app/src/engine/alert.rs`、`app/src/ui/theme.rs`（顏色）、配對卡與列表的 vm／畫面。
- 資料：無資料庫變更、無設定。
- 測試：vm 層文字與色調測試、窮舉 match 的測試（新增身分時編譯期報錯）、GPUI 煙霧測試（若框架可用）。
- 相依：依賴 `live-process`（程式身分）；與 `live-guardrails` 互相獨立，但「交易已鎖定／已武裝」的顯示要等 `live-guardrails` 有武裝狀態才能完整（本變更先顯示 kill switch 現況）。
- 不涵蓋：macOS 系統通知（延後到開 AUTO 之前）、整個 app 的強調色全面換色（只做橫幅與按鈕）、輸入框不可貼上。
