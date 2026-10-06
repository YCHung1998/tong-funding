## 1. 斑馬紋

- [x] 1.1 `theme.rs` 新增 `TABLE_ROW` / `TABLE_STRIPE` / `TABLE_HOVER`，`theme_tests.rs` 加對比度與奇偶亮度比 ≥ 1.2 測試（先紅後綠）
- [x] 1.2 `component_theme.rs` 改用 `Theme::update` 同步 colors → tokens，表底 / 斑馬 / hover 改用新配色；能的話加 `tokens.table_even == TABLE_STRIPE` 測試
- [ ] 1.3 實機截圖確認掃幣表奇偶列底色不同，並逐頁檢查其他元件顏色未異常

## 2. 達標公式

- [x] 2.1 `scanner.rs` 新增 `threshold_breakdown`，於 `scanner_tests.rs` 先寫失敗測試：一般組合（0.3500 範例）、每腿覆寫、最低淨利較嚴、缺少欄位、設定讀取失敗
- [x] 2.2 `ScannerVm` 帶出 breakdown 行，取代原本單行 `threshold_text`
- [x] 2.3 `shell.rs` / `pages.rs` 在頁首渲染公式區塊（可收合）與「於風控設定修改 →」連結

## 3. 驗證

- [x] 3.1 `cargo test -p tong-funding ui::scanner` 與全套 `cargo test` 綠燈
- [ ] 3.2 實機：改風控設定中的滑價，回掃幣頁確認公式數字與結果跟著改變；截圖
