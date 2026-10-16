## 1. 身分與單一實例

- [x] 1.1 新增 `ProcessIdentity { Demo, Live }` 與參數解析（`--live`）；紅色測試先行：預設 Demo、`--live` 為 Live、環境變數不影響、`--live` 搭配 `--bench-*` 被拒、同一個程序內無法改變
- [ ] 1.2 單一實例鎖（資料目錄 `tong-funding.lock`，作業系統檔案鎖）；測試：第二個實例被拒（任一身分組合）、釋放後可重新取得；鎖在開資料庫與啟動引擎之前取得

## 2. 傳輸、主機與限頻

- [x] 2.1 在 `signed/endpoints.rs` 新增 live 主機常數與 `LiveHost` 型別（不加進 `BinanceHost`／`BybitHost`），以及 `LIVE_SIGNED_HOSTS`；新增 `HostPolicy::SignedLive`；測試：Demo 政策拒絕 live 主機、Live 政策拒絕 demo 主機、清單與常數同源
- [x] 2.2 公開政策拒絕所有交易所認證標頭與 `signature`／`sign` 參數；測試涵蓋三家的標頭與查詢參數
- [ ] 2.3 傳輸型別分離（`PublicTransport`／`SignedDemoTransport`／`SignedLiveTransport` 薄包裝）並改簽名客戶端建構子的參數型別；用 `compile_fail` 文件測試或型別測試證明接錯無法編譯
- [ ] 2.4 靜態檢查擴大到整個 `src`（非測試程式碼）掃描正式主機字串，只放行 `signed/endpoints.rs` 與公開客戶端；測試：在其他檔案加入字串會失敗
- [ ] 2.5 限頻分類改依呼叫端型別；測試：同一台主機的公開批次、公開單一、簽名請求各自分類正確

## 3. 資料庫

- [ ] 3.1 `db_venue` 中繼資料、schema 版本 5、`HaltReason::VenueMismatch`、依身分的預設路徑（`funding-live.db`）；測試：新建、環境不符停機、既有 `funding.db` 升級為 demo、舊版 schema 支援值拒絕新資料庫、Live 開無中繼資料的既有資料庫被拒
- [ ] 3.2 命令列工具（`config`、`import` 等）的 `--db` 走同一個開檔規則；測試：Live 身分的工具不能開 demo 資料庫

## 4. 金鑰與訂單 ID

- [ ] 4.1 `BundleSecrets` 依身分選鑰匙圈 service；`secrets` 子命令 `--live`；`import-env` 與 `--live` 互斥；測試用假 `KeyStore`：Live 不讀 demo 項目且不退回、Demo 不讀 live 項目、`import-env --live` 失敗且零寫入
- [ ] 4.2 `IdPrefix::Live` 與 `recovery` 辨識；測試：ID 前綴為 `live`、長度 ≤ 36、重啟對帳辨識 live 單

## 5. 組合根與 OKX

- [ ] 5.1 `main.rs`／`live.rs` 依身分建立「環境套件」（時鐘校時、帳戶、流水、費率、槓桿上限、下單傳輸）；`TONG_FUNDING_BINANCE_HOST` 只對 Demo 有效；測試：用假傳輸記錄目標主機，Live 身分只有 live 主機、Demo 身分只有 demo 主機
- [ ] 5.2 Live 身分排除 OKX：可交易清單、`add_prepared`、`start_entry`、執行器工廠；測試：含 OKX 腿的配對被拒

## 6. 驗證

- [ ] 6.1 `openspec validate live-process` 通過；`cargo test -p tong-funding-core -p tong-funding` 全綠並記錄指令與通過數量；用乾淨的編譯目錄執行（避免共用快取的雜湊衝突）
