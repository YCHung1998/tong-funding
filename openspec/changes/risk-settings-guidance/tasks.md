## 1. 純邏輯（先寫失敗測試）

- [ ] 1.1 `Field::strictness()`，測試：每個欄位分類符合 spec；與 `effective_for_pair` 取 max/min 一致
- [ ] 1.2 `Field::help()`，測試：每個欄位說明非空、含公式的欄位提到對應公式
- [ ] 1.3 `theme.rs` 新增 `STRICT_HIGH`、`STRICT_LOW`，對比度測試

- [ ] 1.4 `stale_data_threshold_ms` 預設 1000 → 3000（`core/src/risk.rs` Default 與相關測試）；`specs/risk-config/spec.md` 以 MODIFIED 複製完整「風控欄位、預設值與驗證」需求並改預設值與說明

## 2. 介面

- [ ] 2.1 全域與每腿覆寫欄位前加方向方框標記
- [ ] 2.3 `stale_data_threshold_ms` 欄位後顯示建議值註解（全文見 proposal）
- [ ] 2.2 `open_dialog` spike；標題旁 `?` 按鈕開啟說明視窗（公式、達標條件、每欄說明、覆寫取較嚴）

## 3. 驗證

- [ ] 3.1 `cargo test -p tong-funding` 全套綠燈
- [ ] 3.2 實機截圖：風控設定頁標記、說明視窗開啟與關閉
