## 1. 解析（先寫失敗測試）

- [x] 1.1 `bybit.rs` 純函式 `available_margin_from`：只取 UNIFIED 帳戶的 totalAvailableBalance、0 與負數原樣、空值回錯誤、不用 availableToWithdraw、CONTRACT 維持 availableToWithdraw；測試涵蓋 spec 全部情境
- [x] 1.2 `get_available_margin()` 走同一次 wallet-balance 請求；錄製回應測試確認 URL 與結果

## 2. 接線

- [x] 2.1 `account.rs` Bybit `available_margin` 改用新方法；更新 `executor_tests.rs:456` 原本的「空值 → 錯誤」測試與 `replay_tests.rs:103` 的錄製回應（加入 `totalAvailableBalance`）

## 3. 驗證

- [x] 3.1 `cargo test -p tong-funding` 全套綠燈
- [x] 3.2 實機（使用者）：交易單頁 Bybit 可用保證金顯示數字，與 Bybit demo 網頁的可用餘額一致（使用者實機確認 2026-10-06）
