## 1. 顏色（先寫失敗測試）

- [x] 1.1 `theme.rs` 新增 rate 正向、負向兩個中性 token（非紅綠、對比度 ≥ 4.5:1）；funding rate 與趨勢的顏色函式改用它們；`theme_tests.rs` 更新；Net Edge 與損益仍用成功／危險色

## 2. 掃幣表

- [x] 2.1 停用交易所的欄預設隱藏（保留使用者手動顯示的選擇）；測試
- [x] 2.2 rate 儲存格分兩行（rate＋週期、8h 等效）、Gross 欄依內容不截斷；headless UI 測試：預設寬度下「達標」「加入交易單」在可視範圍、Gross 標籤不與 Net Edge 重疊
- [x] 2.3 狀態列：開關與來源間隔、每個來源只顯示一次狀態、停用交易所顯示「已停用」弱化文字；測試

## 3. 驗證

- [ ] 3.1 `cargo test` 全套綠燈、`cargo clippy` 無新增警告、`openspec validate scanner-layout --strict` 通過
