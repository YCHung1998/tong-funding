## Why

Bybit 的「可用保證金」在交易單頁一直顯示「未知」，連帶讓 engine 送單前的保證金檢查無法通過。原因：程式以每個幣種的 `availableToWithdraw` 當作可用保證金，但 Bybit 官方文件（`/v5/account/wallet-balance`）註明該欄位「Deprecated for `accountType=UNIFIED` from 9 Jan, 2025」，Unified（UTA）帳戶一律回傳空字串，程式便判定「available USDT balance not reported」。SIMULATION 模式的可用保證金依設計（engine-simulation 決策 6）也取自 demo 帳戶，因此模擬下單同樣受影響。

## What Changes

- `accountType=UNIFIED` 時，Bybit 可用保證金改取帳戶層級 `totalAvailableBalance`（Bybit 自己計算的可開倉餘額，USD 計價）；`"0"` 與負數原樣採用；為空（例如 isolated margin）時維持錯誤（fail closed）。
- `accountType=CONTRACT`（舊帳戶）維持使用 `availableToWithdraw`，行為不變。
- 更新原本鎖定「空 `availableToWithdraw` → 錯誤」的測試為新行為。

## Capabilities

### New Capabilities
- `bybit-available-margin`: Bybit 可用保證金的欄位來源、優先順序與失敗處理。

### Modified Capabilities
（無已封存的相關 spec。）

## Impact

- `app/src/exchange/signed/bybit.rs`：新增 `get_available_margin()` 與純函式解析，附錄製回應測試。
- `app/src/exchange/execution/account.rs`：Bybit 的 `available_margin` 改用新方法；Binance 不變。
- `app/src/exchange/execution/executor_tests.rs`：更新 `account_margin_is_the_available_usdt_and_missing_is_an_error`。
- 影響 engine 送單前保證金檢查的輸入值（現在可算出數字，而非一律失敗）。
