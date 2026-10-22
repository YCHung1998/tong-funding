## Why

畫面評估（2026-10-22，使用者提供 7 頁截圖）發現兩類讓人誤讀或麻痺的問題：
1. **已停用的 OKX 到處報錯**：風控設定已取消勾選 OKX，總覽、交易單、持倉、合約設定仍顯示原始錯誤 `acct_mode_unsupported`；反覆出現的錯誤會讓真正的警告被忽略。
2. **內部代碼直接露出**：`EXCHANGE_DEMO`、`FINALIZED`、`BLOCKED`、`PARTIAL_FAILURE`、`INCOMPLETE` 等。交易單的「上次執行結果」沒有日期（15:59:50 排在 00:59:50 之後）、一筆 `BLOCKED` 的幣種與交易所是空白且沒寫原因（實際是 PTBUSDT 12:00 送單前重抓逾時）、只列訂單編號不列結果。

## What Changes

- **停用交易所的呈現**：不在 `allowed_exchanges` 的交易所，在總覽、交易單、持倉、合約設定只顯示一行弱化文字「OKX 已停用（風控設定未勾選）」，不顯示錯誤細節（細節可展開）；但該所若有持倉 SHALL 照常顯示，曝險不得被隱藏。總資產的說明只列出計入的交易所。
- **白話標示**：集中一份對照（配對狀態、執行環境、觸發模式、PnL 狀態、已知的交易所錯誤碼）→ 中文名稱＋一句說明；畫面顯示中文，內部代碼放在次要文字或說明中。
- **上次執行結果**：每筆顯示日期與時間（UTC）、幣種、中文結果；被擋下時列出原因；已完成的配對顯示淨損益（有 PnL 時）；修正幣種與交易所空白。

## Capabilities

### New Capabilities
- `ui-plain-language`: 停用交易所的呈現、內部代碼的白話對照、上次執行結果的內容。

### Modified Capabilities
（無）

## Impact

- `app/src/ui/vm/`：`dashboard.rs`、`staged_orders.rs`、`positions.rs`、`contract_settings.rs`、`funding.rs`，新增白話對照模組與測試。
- `app/src/ui/pages.rs`、`trading_pages.rs`：顯示調整。
- 不改引擎、不改輪詢、不改事件；狀態列由 `loss-circuit-breaker` 處理，本 change 不碰 `shell.rs` 的狀態列。
