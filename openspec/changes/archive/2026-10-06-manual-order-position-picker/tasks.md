## 1. 純邏輯（先寫失敗測試於 `manual_order_tests.rs`）

- [x] 1.1 `open_positions(snap)`：依 `execution_mode` 選帳戶、濾掉零持倉、失敗 / 不完整狀態
- [x] 1.2 `close_prefill`：多 → SELL、空 → BUY、數量取絕對值、reduce_only = true
- [x] 1.3 `open_orders(snap)` 與 `cancel_prefill`：無 `client_order_id` 者不可選

## 2. 介面

- [x] 2.1 手動下單頁加「目前持倉」清單與「帶入平倉」按鈕，點選寫入 `m_exchange`、`m_symbol`、side、`m_qty`、reduce_only
- [x] 2.2 撤單區加「目前掛單」清單，點選寫入 `x_exchange`、`x_symbol`、`x_id`
- [x] 2.3 失敗 / 不完整 / 無持倉的提示文字與更新時間

## 3. 驗證

- [x] 3.1 `cargo test -p tong-funding ui::manual_order` 與全套 `cargo test` 綠燈
- [x] 3.2 實機（SIMULATION）：先手動開一筆倉 → 清單出現 → 帶入平倉 → 確認 → 持倉歸零；截圖（使用者實機確認 2026-10-06）
