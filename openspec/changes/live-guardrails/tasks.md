## 1. 設定與身分進入引擎

- [ ] 1.1 `core/src/risk.rs`：新增 `live_max_leg_notional_usdt`、`live_max_pairs_per_arming`、程式內建天花板常數與驗證；Live 未填 → 設定不完整；Demo 忽略；紅色測試先行
- [ ] 1.2 `EngineDeps` 新增 `identity`（`ProcessIdentity`）；Demo 的所有新行為關閉；既有測試全綠

## 2. 鎖定與武裝

- [ ] 2.1 Live 啟動時 kill switch 強制 ON（每次啟動）；測試：先前為 OFF 也變 ON、開倉被拒、平倉被接受、Demo 不變
- [ ] 2.2 預檢：可注入的檢查函式（金鑰存在、唯讀餘額與持倉、校時、單向持倉、無既有部位與掛單、保證金、上限、磁碟空間）、引擎產生並保存 `preflight_id`（60 秒有效）；測試：任一項失敗或讀取失敗即不通過、結果不含原始回應
- [ ] 2.3 `Arm` 指令：驗證 `preflight_id`、`caps_hash`、確認字串（NFC＋去空白）、`checklist_ack`；成功時 kill switch OFF、凍結上限、筆數歸零、`LIVE_ARMED` 事件；Demo 拒絕；測試涵蓋每個拒絕情境

## 3. 硬上限與限制

- [ ] 3.1 上限檢查：`RiskLimits` 加入配對名目檢查；`OrderRequest` 建立後以「數量 × 最差保護價」檢查所有非 reduce-only 送單；加入交易單與合約模板儲存檢查；reduce-only 不受限；測試涵蓋超限被拒、平倉不受限、天花板
- [ ] 3.2 一次武裝筆數上限：累計進場數、達上限自動鎖定；測試：第 2 筆完成後鎖定、第 3 筆被拒
- [ ] 3.3 Live 手動下單：非 reduce-only 被拒；新增 `FlattenLeg { pair, leg }`（重用 `plan_leg_close`，kill switch ON 時允許）；測試：手動開倉被拒、`PARTIAL_FAILURE` 時只平裸露的一腿
- [ ] 3.4 自動鎖定：集中函式 `lock_live(reason)`；`PARTIAL_FAILURE`／`IMBALANCED`／`UNRESOLVED`、認證類錯誤、HTTP 418／429、校時失敗各有測試；鎖定不阻擋平倉
- [ ] 3.5 設定凍結：武裝後調高被拒、調低生效；`config set` 對 live 資料庫拒絕寫入受保護鍵；測試涵蓋
- [ ] 3.6 Live 資料庫初始值（`MANUAL`、`early_exit_min_move_pct = 0`、模板名目不超過上限）；Live 拒絕 `trigger_mode = AUTO`；測試涵蓋

## 4. 畫面與連結

- [ ] 4.1 「前往交易所」連結的純函式（網址只含公開資訊）與測試；配對卡與橫幅按鈕（與 `live-banners` 的頁面框架整合，整合階段完成）
- [ ] 4.2 武裝對話框（輸入確認字串、勾選人工清單、顯示預檢結果與上限，整合階段完成）

## 5. 驗證

- [ ] 5.1 `openspec validate live-guardrails` 通過；`cargo test -p tong-funding-core -p tong-funding` 全綠並記錄指令與通過數量；用乾淨的編譯目錄執行
