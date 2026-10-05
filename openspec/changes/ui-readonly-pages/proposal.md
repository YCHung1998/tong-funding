## Why

使用者最核心的需求是「監控倉位、交易單與持倉行為、策略使用紀錄」。這個 change 做完四個唯讀頁面與全頁警示橫幅，
是第一次能對真實 demo 帳戶看到資料的里程碑，也最早暴露 adapter 與版面的問題。

## What Changes

- **總覽**：各所總資產與占比（錢包資產 + 合約權益，不含持倉名目本金）、資產價值分布與持倉保證金分布兩張甜甜圈圖、各所資產明細表、曝險摘要；未連線與僅比價的交易所明確呈現，不當成零。
- **持倉**：交易所與幣種篩選、持倉表、對沖配對卡片（依 `position-grouping`）、配對狀態與不平衡率、未配對標示。「Funding 收到」欄先留位置，由 `funding-pnl` 補資料。
- **系統日誌**：事件時間軸、事件類型多選篩選（選項取自資料）、完整結構化 JSON 詳情；`SCAN_RUN` 來自記憶體緩衝並標示「僅本次運行」；舊事件標示「匯入」；分頁載入。
- **掃幣**：每所 rate 旁顯示 funding 週期標籤、Gross Spread 與 Net Edge 兩欄（缺設定顯示「未設定」，不是 0）、「只顯示達標」toggle 與符合筆數、各標的獨立結算倒數、立即刷新（重新抓取資料並重算，不得只刷新畫面）、移除 Pionex / Bitget 佔位欄、OKX 顯示公開行情並標「僅比價」。
- **全頁警示橫幅**：需人工處理（`PARTIAL_FAILURE` / `IMBALANCED` / `UNRESOLVED`）、停機、交易所斷線、資料過期、限流與時鐘的常駐提示；資料新鮮度指示、載入中 / 錯誤 / 空狀態的明確區分。
- 與 Figma 的逐項差異登記於 `design.md` 的「Figma 差異對照表」。

**不在本 change 範圍**：任何會改變狀態的操作（下單、平倉、設定修改）；掃幣頁的「加入交易單」與 Candidate List（見 design.md Open Questions #2）；總覽的 24 小時趨勢圖；macOS 系統通知與單腿失敗事件的寫入（`exchange-demo-execution`）；合約設定、交易單、風控設定、手動下單四頁（`ui-trading-pages`）。

## Capabilities

### New Capabilities
- `dashboard-page`: 總覽頁（彙總卡、估值規則、各所卡片與兩張分布圖、資產明細、曝險摘要、未連線處理）。
- `positions-page`: 持倉頁（篩選、彙總卡、持倉表與 Funding 欄位、配對卡片、未配對標示、資料不完整提示）。
- `system-log-page`: 系統日誌頁（時間軸、事件來源合併、類型篩選、完整 JSON、恢復事件）。
- `scanner-page`: 掃幣頁（列集合與欄位、週期標籤、Gross / Net Edge、達標與排序、OKX 僅比價、結算倒數、toggle、立即刷新、頁首狀態、重算頻率）。
- `alert-banner`: 全頁警示橫幅、警示類別與排序、人工處理與停機警示、來源層級判定、資料新鮮度指示。

### Modified Capabilities
<!-- 無 -->

## Impact

- 依賴 `bootstrap-gpui-shell`（殼、theme、元件可行性）、`core-domain-and-fixtures`（Net Edge、達標、風控合併、配對分組、Pair 狀態）、`store-sqlite`（事件、配對、設定、停機旗標、`SCAN_RUN` 緩衝）、`exchange-readonly-adapters`（行情、帳戶資料、健康狀態、校時、重新抓取）。
- 系統日誌的「恢復」顯示依賴 `exchange-readonly-adapters` 的 `feed-health` 寫入獨立的 `FEED_RECOVERED` 事件。
- 在 `ui-trading-pages` 之前，使用者沒有 UI 可輸入費率與門檻，Net Edge 與達標會顯示「未設定」（design.md Open Questions #1）。
