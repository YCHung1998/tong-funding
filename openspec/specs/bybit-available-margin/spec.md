# bybit-available-margin Specification

## Purpose
TBD - created by archiving change bybit-uta-available-margin. Update Purpose after archive.
## Requirements
### Requirement: Unified 帳戶的可用保證金來源

`accountType=UNIFIED` 時，Bybit 可用保證金 SHALL 取 `result.list` 中 `accountType` 為 `UNIFIED` 的帳戶的 `totalAvailableBalance`（Bybit 自己計算的可開倉餘額）。
該值為 `"0"` 或負數時 SHALL 原樣採用（不得改用其他來源）。
找不到 UNIFIED 帳戶、或 `totalAvailableBalance` 缺少或為空字串時，SHALL 回傳錯誤，訊息 SHALL 指出缺少的欄位；SHALL NOT 以 0、估計值或其他欄位代替。
系統 SHALL NOT 在 UNIFIED 帳戶使用 `availableToWithdraw`（Bybit 已於 2025-01-09 起停用）。

#### Scenario: 使用帳戶可用餘額

- **WHEN** UNIFIED 帳戶的 `totalAvailableBalance` 為 `"8123.45"`，USDT 的 `availableToWithdraw` 為 `""`
- **THEN** 可用保證金為 8123.45

#### Scenario: 零與負數原樣採用

- **WHEN** `totalAvailableBalance` 為 `"0"`（或 `"-12.5"`）
- **THEN** 可用保證金為 0（或 −12.5），送單前保證金檢查不通過

#### Scenario: 帳戶可用餘額為空

- **WHEN** `totalAvailableBalance` 為 `""`（例如 isolated margin）
- **THEN** 回傳指出 `totalAvailableBalance` 的錯誤，交易單頁顯示「未知（…）」，送單前保證金檢查不通過

#### Scenario: 不使用已停用欄位

- **WHEN** `totalAvailableBalance` 為 `""`，USDT 的 `availableToWithdraw` 為 `"999"`
- **THEN** 回傳錯誤，而非 999

### Requirement: CONTRACT 帳戶維持原行為

`accountType=CONTRACT` 時，可用保證金 SHALL 仍取 USDT 的 `availableToWithdraw`；為空時 SHALL 回傳錯誤。

#### Scenario: 舊帳戶

- **WHEN** 帳戶類型為 CONTRACT，USDT 的 `availableToWithdraw` 為 `"90"`
- **THEN** 可用保證金為 90

