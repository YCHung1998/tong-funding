## 1. 工作區與可建置性

- [x] 1.1 把根目錄 `Cargo.toml` 改為 workspace（members：`core`、`app`），`src/main.rs` 移到 `app/src/main.rs`；執行 `cargo build`，記錄成功與否、耗時、警告數
- [x] 1.2 `gpui-kit` 鎖定 `=0.7.1`；`cargo build` 通過後把輸出貼進 PR 描述（若失敗，停止後續 task 並回報，不要硬改）
- [x] 1.3 建立 `core` crate 與一個 trivial 測試；執行 `cargo test -p core`，並用 `cargo tree -p core` 證明依賴樹無 `gpui` / `gpui-kit`
- [x] 1.4 用 `cargo tree -p app` 證明沒有 `reqwest`、`hyper`、`tokio-tungstenite`、`ureq`

## 2. Theme 與字型

- [x] 2.1 建立 theme 模組（色票、字級常數）；寫測試：色值與 `design-tokens` spec 列出的清單逐一相同
- [x] 2.2 寫測試：掃描 theme 以外的原始碼，不得出現色碼字面值
- [x] 2.3 實作 funding rate 顏色與趨勢箭頭語意函式，先寫測試（三種 rate、三種趨勢）並確認紅燈，再實作到綠燈
- [x] 2.4 打包 IBM Plex Mono（中文改用系統字型，見 design.md「字型實測」）；截圖證明「持倉 Unified Positions 60,200.00 USDT」無缺字方塊；記錄字型檔大小與授權

## 3. 視窗殼

- [x] 3.1 實作標題列（UTC 與 Taipei 雙時鐘，每秒更新）、側邊欄 8 項（含分隔線與手動下單警示）、狀態列（模式徽章預設 `SIMULATION`、三所「未連線」、kill switch「未啟用」）、8 個空白頁路由；`cargo run` 截圖附上

## 4. 可行性 spike

- [x] 4.1 基準頁：528 列 DataTable，更新頻率可調（1 / 2 / 10 Hz），輸出 p50、p95 幀時間；把結果填進 `design.md` 量測紀錄表
- [x] 4.2 甜甜圈圖 spike：以 `PieChart` 渲染總覽 Figma 的資產分布資料，截圖並記錄是否可行
- [x] 4.3 依量測結果判定是否達預算；未達標則在 `design.md` 記錄緩解決策並通知使用者
