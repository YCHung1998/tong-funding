## 1. core：達標判定（先寫失敗的測試，再實作）

- [ ] 1.1 在 `core/tests/net_edge_qualify.rs` 新增紅色測試：兩腿結算時間不同且 Net Edge 遠高於門檻 → 不達標（Net Edge 數值仍依單次結算模型算出）；兩腿結算時間相同且達門檻 → 達標；一腿 `next_funding_time` 為 0 → 不達標；兩個方向結果相同；確認先前此情境是「達標」（紅）
- [ ] 1.2 在 `core/src/net_edge.rs` 新增 `legs_settle_together(long, short)`（兩腿 `next_funding_time` 相等且大於 0），並把它加進 `evaluate_oriented` 的 `qualifies`；1.1 轉綠
- [ ] 1.3 跑 `core` 全部測試（含 `core/tests/net_edge_compute.rs`、`parity_fixtures.rs`、`min_expected_net_pnl.rs`）；凡是「單邊結算且達標」的案例改寫成不達標，並保留 Net Edge 數值的斷言，不得直接刪除

## 2. 送單前檢查（node0）

- [ ] 2.1 在 `app/src/engine/node0.rs` 的測試新增紅色測試：其餘檢查通過、淨邊際達標，但送單前最新資料兩腿 `next_funding_time` 不同 → BLOCK 且失敗清單只有 `NetEdgeQualified` 並附說明；一腿缺失 → 同樣 BLOCK；兩腿相同 → PASS
- [ ] 2.2 在 `net_edge_qualified` 判定中呼叫同一個 `legs_settle_together`，不同步時在 `notes` 寫明兩腿下次結算時間；2.1 轉綠；確認 `core/tests/pretrade_checks.rs` 仍綠

## 3. 掃幣頁與交易單入口

- [ ] 3.1 在 `app/src/ui/vm/scanner_tests.rs`（或同層的測試檔）新增紅色測試：兩腿結算時間不同的列 → `Qualified::NotSynchronized`、顯示「結算不同步」；`candidates::eligibility` 回傳不可勾選；自動加入不會選到它；兩腿同步且達標的列仍為 `Qualified::Yes`
- [ ] 3.2 在 `app/src/ui/vm/scanner.rs` 新增 `Qualified::NotSynchronized`（文字「結算不同步」），`pairing` 在「淨邊際等條件不論、兩腿結算時間不同」時回傳它；`eligibility` 將它視為不可勾選；補齊所有對 `Qualified` 的完整比對（編譯器會指出）；3.1 轉綠

## 4. 驗證

- [ ] 4.1 `cargo test -p tong-funding-core -p tong-funding` 全綠，記錄指令與通過數量
- [ ] 4.2 `openspec validate require-synchronized-settlement` 通過
- [ ] 4.3 用實際資料核對（唯讀）：對 `funding.db` 中 2026-10-14／15 八筆已進場配對的進場快照重新套用新規則，確認「兩腿結算時間不同」的 3 筆（RSR 02:00、RSR 04:00、BLUR 04:00）會被擋、其餘 5 筆維持可進場，結果記錄在 `issue/`
