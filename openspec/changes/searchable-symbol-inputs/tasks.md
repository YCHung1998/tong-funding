## 1. 純邏輯（先寫失敗測試）

- [x] 1.1 `symbol_options(snap, exchange)`、`all_symbol_options(snap)`、`coin_options(snap)`：去重、字母序、行情未載入回傳空
- [x] 1.2 `options_with_query(query, options)`：不分大小寫子字串過濾、query 不在清單時首列為自由輸入項

## 2. 元件 spike

- [x] 2.1 以最小範例確認 `SelectState` searchable + 自訂 delegate 能動態插入「使用 <query>」並取回值

## 3. 替換欄位

- [x] 3.1 合約設定「試算標的」改為 searchable Select（所有交易所聯集）
- [x] 3.2 手動下單 Symbol 改為 searchable Select，交易所切換時更新候選；`manual-order-position-picker` 的帶入改用 `set_selected_value`
- [x] 3.3 撤單 Symbol 改為 searchable Select（候選為掛單 Symbol）
- [x] 3.4 風控 `allowed_coins` 改為 multiple + searchable Combobox，儲存格式不變
- [x] 3.5 不在清單中的值顯示「不在目前行情清單中」提示

## 4. 驗證

- [x] 4.1 `cargo test` 全套綠燈（含既有 manual_order / risk_settings / contract_settings 測試）
- [ ] 4.2 實機（部分完成）：已截圖確認合約設定下拉可捲動、手動下單/撤單 Symbol 顯示與「不在目前…清單中」提示；沙箱無法送出鍵盤輸入，「輸入一半字串過濾」與「風控 allowed_coins」未實機驗證（以 headless 測試涵蓋）
