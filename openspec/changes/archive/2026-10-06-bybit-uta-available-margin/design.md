## Context

- `BybitSignedClient::get_balances()` 只解析每個幣種（`parse_balance`），`available` 取 `availableToWithdraw`；`account.rs::to_margin` 找 USDT 的 `available`，沒有就回錯誤。
- Bybit 文件：`availableToWithdraw` 對 UNIFIED 自 2025-01-09 停用；帳戶層級 `totalAvailableBalance`（USD）= cross: `totalMarginBalance − Haircut − totalInitialMargin`，portfolio: `totalEquity − Haircut − totalInitialMargin`；isolated margin 下帳戶層級欄位不適用（空）。幣種層級有 `walletBalance`、`totalPositionIM`、`totalOrderIM`、`locked`。
- SIMULATION 的可用保證金依設計取自 demo 帳戶（`engine/ports.rs` 註解、`sim.rs` `MarginFromAccount`）。
- 交易單頁 `staged_orders.rs::margin_text` 顯示 `leg_accounts[(false, ex)].available_margin`。

## Goals / Non-Goals

**Goals:** UNIFIED 帳戶能取得可用保證金；仍 fail closed。

**Non-Goals:** 改 Binance；改 SIMULATION 取 demo 保證金的設計；引入「模擬帳戶自有餘額」。

## Decisions

1. **新方法 `get_available_margin() -> Result<Decimal, AdapterError>`**，解析為純函式 `available_margin_from(body, account_type)`，以錄製 JSON 測試；`Balance` 結構與 `get_balances` 不變（`Balance` 是逐幣種，帳戶層級 USD 值不屬於它）。
2. **只用 `totalAvailableBalance`，不做 USDT 計算 fallback**（多方抗辯後修正，原提案的 fallback 已移除）：`walletBalance − IM − locked` 不含未實現虧損，且 portfolio margin 的幣種 IM 欄位為空會被當成 0，兩者都會高估可用保證金（fail open）。isolated margin 目前不使用，空值直接回錯誤。
3. **只讀 `accountType` 相符的那一筆帳戶**，避免多筆 list 時取錯或重複計算。
4. **`"0"`、負數原樣採用**：負值必然使送單前保證金檢查失敗，原樣顯示比夾成 0 更能看出原因。
5. **USD 視同 USDT**：`totalAvailableBalance` 為 USD 計價並含扣 haircut 後的非 USDT 抵押品；這就是 Bybit 允許開倉的額度。精確換算與安全邊際不在本 change 範圍（見風險）。
6. **AccountView 介面不變**：`available_margin` 仍回 `Result<Decimal, String>`，UI 與 engine 不需改動。

## Risks / Trade-offs

- [Bybit 腿第一次能通過保證金檢查，暴露既有缺口] → 抗辯指出：`margin_needed = notional / leverage` 沒有手續費與緩衝、程式不設定交易所槓桿（實際 IM 可能更高）、同時進入 PRE_TRADE_CHECK 的多個 pair 不會互相保留保證金。這些是既有問題（之前 Bybit 一律失敗所以碰不到），列為後續 change，不在本次修正。
- [USD 與 USDT 脫鉤、抵押品價格變動] → 同上，後續以保證金緩衝處理。
- [無法用真實帳戶在 agent 環境驗證] → 以文件範例格式的錄製回應測試；實機由使用者確認交易單頁顯示數字（task 3.2）。
