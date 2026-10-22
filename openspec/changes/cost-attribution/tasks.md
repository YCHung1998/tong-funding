## 1. 先查清楚參考價來源

- [x] 1.1 查證開倉 `expected_price` 與平倉 `reference_price` 的價格種類（mark / last / ticker）與取得路徑，把結論寫回 `design.md` 的 Open Questions；若不同源，`attribution` 要包含 `reference_sources`

## 2. 核心（先寫失敗測試）

- [x] 2.1 `core/src/cost_attribution.rs`：由 `PairInput` 算出進場滑價、出場滑價、漂移、資金費、手續費與每腿名目 %；測試涵蓋 spec 數字情境、恆等式（對照 `compute_pnl` 的 `slippage`／`price_ref`／`net`）、缺資料為未知
- [x] 2.2 `core/src/edge_kind.rs`：型態與每小時費率差；測試涵蓋 spec 四個情境（週期錯位、週期相同、週期不同但費率差大、無收入）

## 3. 接進引擎

- [x] 3.1 `app/src/funding/pnl_record.rs`：`pnl_payload` 增加 `attribution`；測試確認既有 `breakdown`／`status`／`slippage` 值不變，且重算去重行為不變
- [x] 3.2 `app/src/engine/node0.rs`：進場快照 `net_edge` 增加 `edge_kind` 與 `hourly_spread_pct_per_hour`；測試確認既有欄位不變

## 4. 介面

- [x] 4.1 掃幣頁 view-model 與畫面：型態標籤與說明文字；測試確認不影響達標篩選與排序

## 5. 報表

- [x] 5.1 `issue/tools/cost_report.py`：逐筆表、佔比、分位數、建議值、樣本數警示、唯讀；合成資料庫測試（含「不寫入」與「舊資料還原與新欄位一致」）
- [ ] 5.2 以目前的 `funding.db` 實跑一次（唯讀），把輸出寫成 `issue/007-cost-attribution-baseline.md` 並更新 `issue/README.md`

## 6. 驗證

- [ ] 6.1 `cargo test` 全套綠燈、`cargo clippy` 無新增警告、`openspec validate cost-attribution --strict` 通過
