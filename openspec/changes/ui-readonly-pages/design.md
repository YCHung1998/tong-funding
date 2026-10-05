## Context

來源：Figma 解碼文字（總覽、掃幣、持倉、系統日誌四個畫面，逐字比對）、`bootstrap-gpui-shell`（色票、字型、`gpui-component` 的表格與圖表元件、效能預算）、`mvp-python`（`app.py` 的對應頁面、`portfolio_value.py`、`position_grouping.py`、`reference/event_log_schema.md`）、`exchange-readonly-adapters`（資料與健康狀態）、`core-domain-and-fixtures`（Net Edge、達標、風控合併、配對分組、Pair 狀態）、`store-sqlite`（事件表、`SCAN_RUN` 緩衝、停機）。

**已驗證的事實**

| 項目 | 事實 | 來源 |
|---|---|---|
| 設計稿尺寸與主題 | 固定 1600px 寬、深色；色票與字型已在 `bootstrap-gpui-shell` 定義，本 change 不重寫色碼 | `bootstrap-gpui-shell/design.md` |
| 表格與圖表元件 | `gpui-component 0.7.1` 有 `table`（內部 `uniform_list` 虛擬化）與 `chart/pie_chart.rs`；**尚未編譯**，甜甜圈圖 spike 結果待 `bootstrap-gpui-shell` task 4.2 | 同上 |
| Python 版總資產把持倉名目計入 | `portfolio_value.compute_binance_total` 與 `compute_bybit_total` 在餘額之外又加上持倉的 `|數量| × 價格`；Figma 的註腳明確寫「未重複計入合約名目本金」，兩者不同 | `portfolio_value.py`、Figma 總覽 |
| Python 版 Binance 餘額加總未換算 | `compute_binance_total` 直接加總 `balance`，非 USDT 資產沒有乘價格 | `portfolio_value.py` |
| Python 版持倉頁欄位 | Binance `unRealizedProfit`、Bybit `unrealisedPnl`、OKX `upl` 為交易所回報的未實現損益，頁面直接使用 | `app.py`（持倉頁） |
| 事件表現況 | 舊 `events.jsonl` 7,129 行（`SCAN_RUN` 4,844、`FETCH_ERROR` 1,168）；匯入後預期約 2,285 筆；**系統日誌頁必須能處理數千筆以上** | `store-sqlite/design.md` |
| Figma 日誌的「已恢復」 | Figma 的 `FETCH_ERROR` 事件 payload 內含 `recovered_at` 與 `status: RECOVERED`（同一筆事件） | Figma 系統日誌 |
| 資料來源的更新週期 | Binance 1 秒 WebSocket；Bybit、OKX 輪詢（Python 版 10 秒）；帳戶輪詢（Python 版 30 秒）；Figma 掃幣頁有「Bybit 下次刷新 12s」、總覽有「帳戶刷新 12s」 | `exchange-readonly-adapters/design.md`、Python `app.py` |
| 配對來源 | `position-grouping` 只配對狀態為 `RECONCILED` 的配對；在 `engine-simulation` 之前，store 的 `pairs` 表不會有新建的 `RECONCILED` 配對（舊事件匯入只匯入事件，不匯入 `pairs`） | `core` 與 `store-sqlite` 的 specs |

## Goals / Non-Goals

**Goals**
- 第一次能用真實 demo 帳戶與真實行情看到四個頁面，且每個數字都能追溯到 `core` 的函式或交易所回報值。
- 逐頁對照 Figma，並把每一處刻意的差異登記在下表。
- 警示橫幅讓「資料過期、斷線、停機、需人工處理」在任何頁面都不會被漏看。

**Non-Goals**
- 不做任何會改變狀態的操作：不下單、不平倉、不修改設定。
- 不做「加入交易單」按鈕與 Candidate List（Figma 掃幣頁下半部）：它需要寫入暫存配對，屬於 `ui-trading-pages` 之後的流程（見 Open Questions #2）。
- 不做總覽的 24 小時趨勢圖（Figma 沒有、`portfolio_history` 的寫入尚未排入任何 change）。
- 不做 macOS 系統通知與「單腿失敗」事件的寫入（屬於 `exchange-demo-execution`）；本 change 只負責讀取並顯示。
- 不做合約設定、交易單、風控設定、手動下單四頁（`ui-trading-pages`）。

## Decisions

**D1　頁面分成「view-model 純函式」與「GPUI 繪製」兩層。**
view-model 輸入是 adapter 的輸出、`core` 的結果與設定，輸出是純資料結構（沒有任何 GPUI 型別），放在 `app` 內的獨立模組，用 `cargo test -p tong-funding` 測試。
理由：Net Edge 欄、達標、倒數、占比、篩選、警示排序都是會出錯的邏輯，必須能不啟動視窗就驗證；繪製層只能用截圖對照 Figma 驗證，成本高。

**D2　資料橋接：`ReadOnlyDataSource` 產生 `UiSnapshot`，之後由 `engine-simulation` 的 Snapshot 取代。**
`tasks.md` 1.1 定義這個介面，目前由 adapter 的輪詢與 WebSocket 餵入。UI 只讀 `UiSnapshot`，不直接呼叫 adapter（「立即刷新」除外，它是一個明確的指令，經由資料來源執行）。
更新合併頻率上限預設 2 Hz（`bootstrap-gpui-shell` D5 以 2–10 Hz 為前提，我選下限；**未驗證**），倒數以獨立的 1 Hz 計時，不觸發重算。

**D3　Net Edge、達標、方向、配對分組、配對狀態一律呼叫 `core`，頁面不實作。**
掃幣表每列以 `core` 的函式計算；Net Edge 的百分比結果與名目本金 N 無關（公式各項皆與 N 成正比），所以掃幣表不需要合約設定。

**D4　「可下單的交易所」是單一常數集合（Binance、Bybit），OKX 只比價。**
達標與最佳方向只在該集合內計算，OKX 的 rate 顯示但不參與。這是對拍板「OKX 欄顯示公開行情但標『僅比價』」的解讀：OKX 不能下單，若讓它參與達標，使用者會看到永遠無法執行的「達標」。（Open Questions #3）

**D5　掃幣頁的 Net Edge 門檻唯讀顯示，不在掃幣頁編輯。**
Figma 有一個可編輯的「達標門檻 %」輸入框（舊的 gross spread 門檻）。達標已改由風控設定的 `net_edge_threshold_pct` 決定，且該值可被每腿覆寫、與風控頁共用；在兩個頁面都能編輯會造成兩個來源。因此掃幣頁只顯示並連到風控設定。（Open Questions #1、#4）

**D6　警示由單一純函式產生，來源層級的過期判定沿用 `feed-health`。**
理由見 `exchange-readonly-adapters` D4：若橫幅用每列 1 秒門檻判斷，輪詢型來源會一直閃爍。需人工處理與停機不可關閉，其他警示隨條件消失。未知狀態視為異常。

**D7　rate 顯示到小數 4 位，而非 Figma 的 3 位。**
實測的真實 rate 例如 0.00005047（= 0.0050%），3 位小數會顯示為 0.005，把 0.0045% 與 0.0054% 都吃成同一個數；Net Edge 的門檻在百分之幾的尺度，4 位較不易誤導。已登記於差異表。

**D8　不平衡率公式定義在 view-model，不在 `core`。**
`core` 的 `pair-lifecycle` 只定義了「`max_leg_imbalance_pct`」欄位，沒有定義不平衡率的算法。本 change 暫定 `|Δ數量| ÷ max(數量)`（我的提議，**未驗證其與 Python 版 `trade_pipeline.py` 的 tolerance 算法一致**），僅用於顯示標籤。若之後引擎需要同一個公式，應移到 `core`。（Open Questions #5）

**D9　總資產採「錢包資產 + 合約權益」，不含持倉名目本金。**
依 Figma 註腳。與 Python 版的差異是刻意的（見差異表）。各所 API 欄位到「資產列」的對應（尤其 Binance 多資產模式、Bybit UNIFIED 的 `usdValue` 與已用保證金）**未驗證**，須由 task 4.1 的真實 demo 帳戶決定，並更新本表。

**D10　系統日誌分頁大小暫定 500 筆（沿用 Python 版的 `limit=500`），以 `ts_ms DESC` 與游標載入。**
**未驗證**這個大小在 GPUI 下的繪製成本；若過重，改為 200。記憶體的 `SCAN_RUN` 緩衝（上限 200，`store-sqlite` D4）與資料庫查詢結果在 view-model 內依時間合併。

**D11　事件的「已恢復」改為獨立事件。**
`events` 不可更新（`store-sqlite` D3），所以 Figma 同一筆事件內的 `status: RECOVERED` 不可實現；`exchange-readonly-adapters` 寫入 `FETCH_ERROR` 與 `FEED_RECOVERED` 兩筆。頁尾的「（已恢復）」說明改為各類型獨立計數。

**D12　持倉頁的配對卡片在引擎出現之前不會有資料。**
`position-grouping` 需要 `RECONCILED` 配對；本 change 之前沒有任何程式會建立它。因此對真實 demo 帳戶驗證時，所有持倉會顯示「未配對」，這是預期，不是錯誤。配對卡片的 view-model 以測試用的假配對驗證。（Open Questions #7）

## Figma 差異對照表

| 頁面 | Figma 元素 | 本 change 的處理 | 原因 |
|---|---|---|---|
| 全部 | 標題「Paper Trading MVP · v0.2」、「NO LIVE KEYS · SIMULATION」、「🟢 Demo mode — Binance + Bybit」 | 改為 Demo / Testnet 標示與目前模式徽章（`SIMULATION` / `EXCHANGE_DEMO`），由 `bootstrap-gpui-shell` 的殼處理 | 已拍板，不使用 LIVE 或 Paper 字樣 |
| 全部 | 頁尾「靜態示範資料 · 所有控制、時鐘與訂單結果皆為設計範例」與「SNAPSHOT 07:42:18 UTC」 | 移除示範字樣；「SNAPSHOT」改為實際資料時間 | 不是示範資料 |
| 全部 | 無警示橫幅 | 新增（`alert-banner`） | 拍板新增 |
| 總覽 | 「CONNECTED · PAPER」 | 「CONNECTED · DEMO」或「TESTNET」；未連線顯示原因 | 同上 |
| 總覽 | 「USDT · Binance + Bybit」 | 「Binance + Bybit」並註明 OKX 僅比價；未連線的所不計入並註明 | 失敗即封閉、不把未知當零 |
| 總覽 | Total 為 Binance 加 Bybit | 同左，但定義改為「錢包資產 + 合約權益」，明確不計名目 | Figma 註腳；Python 版有誤 |
| 總覽 | 無 OKX | 新增 OKX 說明卡「僅比價，不提供帳戶資料」 | OKX 無簽名端點 |
| 總覽 | 「Open Positions 4 · 2 對避險組合」 | 同左；無配對時顯示「0 對避險組合」並標示未配對 | 配對來自 `RECONCILED`（D12） |
| 掃幣 | 「達標門檻 %」可編輯輸入框 | 唯讀顯示 `net_edge_threshold_pct` 並連到風控設定 | D5 |
| 掃幣 | 「□ 只顯示達標」＋「Filter : … (ai 要幫我修)」 | 明確 toggle 並顯示「符合 N 筆」 | 拍板 |
| 掃幣 | 副標「以資金費率差排序」；「NEXT SETTLEMENT 08:00 UTC」（單一全域） | 預設依 Net Edge 排序；移除全域結算時間，改為每列倒數 | 各標的週期不同 |
| 掃幣 | 欄位 Pionex、Bitget 佔位欄 | 移除 | 拍板 |
| 掃幣 | 欄位 Binance / Bybit rate | 每所 rate 旁加週期標籤；4 位小數 | 拍板；D7 |
| 掃幣 | OKX 欄「N/A」 | 顯示公開行情 rate 與週期，標「僅比價」 | 拍板 |
| 掃幣 | 「Max / Spread %」一欄 | 拆成 Gross Spread % 與 Net Edge % 兩欄 | 拍板 |
| 掃幣 | 「雙所覆蓋 20 · Binance + Bybit · 2/2」 | 「多所覆蓋 N」，覆蓋欄為 n/（啟用的所數） | 三所 |
| 掃幣 | 「達標（可執行）12 · Spread ≥ 0.030% · Demo 預檢通過」 | 「達標 N · Net Edge ≥ 門檻」；移除「預檢通過」 | 預檢只在送單時進行 |
| 掃幣 | 「Bybit 下次刷新 12s」與「● Binance WS · ONLINE」 | 保留並加入 OKX 與實際狀態（ONLINE / RECONNECTING / OFFLINE） | 真實連線狀態 |
| 掃幣 | 「加入交易單？」欄與下半部 Candidate List、「加入並前往交易單 →」 | **本 change 不做** | Non-Goals；Open Questions #2 |
| 持倉 | 篩選器（Exchange、Coin）與「顯示 4 / 4」 | 保留；選項取自實際持倉 | |
| 持倉 | 欄位 Exchange … Unrealized PnL | 新增「Funding 收到」欄（先為「—」） | 拍板，由 `funding-pnl` 補 |
| 持倉 | 「中性組合 · HEDGED · 0.00% IMBALANCE」卡片 | 保留；配對狀態異常時改以警示色顯示狀態與「需人工處理」 | 完全人工處理 |
| 持倉 | 「PnL 未扣手續費與資金費」 | 保留，直到 `funding-pnl` | |
| 持倉 | 無 OKX | 註明「OKX 僅比價，不顯示持倉」；Python 版的「OKX 單位是合約張數」提示不再需要 | OKX 無帳戶資料 |
| 系統日誌 | 「READ ONLY · DEMO」與「全部來自 Paper Trading 範例」 | 「READ ONLY」；移除範例說明 | 真實資料 |
| 系統日誌 | 事件標籤多選（ORDER_SUBMITTED / SCAN_RUN / FETCH_ERROR） | 選項取自資料；`SCAN_RUN` 標「僅本次運行」 | 拍板；D10 |
| 系統日誌 | `FETCH_ERROR` payload 含 `recovered_at` | 改為獨立 `FEED_RECOVERED` 事件 | D11 |
| 系統日誌 | 無「匯入」標示 | 舊事件加「匯入」標示 | 舊 `events.jsonl` 已匯入 |

## Risks / Trade-offs

- **在 `ui-trading-pages` 之前，使用者沒有地方輸入費率與門檻。** Net Edge 與達標在設定完成前一律顯示「未設定」，這是正確行為，但 task 4.1 的真實 demo 驗證需要設定值；見 Open Questions #1。
- **在引擎出現之前，持倉頁與總覽的配對區塊沒有真實資料**（D12）。Python 版留下的 demo 持倉會全部顯示為「未配對」與「未避險單腿」警示，可能造成誤解；已在頁面標示原因。
- **甜甜圈圖元件尚未驗證可行。** 若 `bootstrap-gpui-shell` task 4.2 失敗，總覽的兩張圖需要改以自繪或其他圖形（長條）替代，spec 的圖表內容要求（占比與數字）不變。
- **528 列與 1 秒 WebSocket 的幀時間未知。** 預算來自 `bootstrap-gpui-shell`（p95 ≤ 16.7 ms，**我的提議、未驗證**）；task 4.2 量測，未達標則依該 change 的緩解順序處理。
- **view-model 與 `core` 的邊界。** 不平衡率（D8）是一個漏洞：它是業務規則，卻因 `core` 沒有定義而落在 UI 層。
- **資產列的對應（D9）未驗證**，總覽的數字在真實帳戶上可能與交易所自己的畫面不一致，需要在 task 4.1 對照交易所介面確認。

## Open Questions

1. **使用者要在哪裡輸入費率與門檻？**（需要使用者決定）`ui-trading-pages`（含風控設定頁）在本 change 之後。選項：(a) 先實作一個開發用的設定寫入子命令（不列入 spec，只供驗證）；(b) 把風控設定頁提前到本 change；(c) 驗證時直接寫入 SQLite 測試資料。我傾向 (a)，但這會讓「真實 demo 驗證」依賴一個非正式入口。
2. **「加入交易單」按鈕與 Candidate List 目前不屬於任何 change。** Figma 掃幣頁有它們；本 change 排除、`ui-trading-pages` 的 proposal 沒有列入。需決定放在哪個 change（建議 `ui-trading-pages`，因為它需要寫入暫存配對）。
3. **含 OKX 的機會要不要提示？** 目前達標與方向只在 Binance、Bybit 之間計算，OKX 完全只比價。若 OKX 的 rate 讓某標的出現更大的 Gross Spread，使用者不會看到任何提示。是否需要「OKX 比價提示」標示，請使用者決定。
4. **掃幣頁的門檻是否仍要可編輯？** D5 把它改為唯讀。若使用者希望保留 Figma 的即時調整體驗，需決定它改寫的是全域 `net_edge_threshold_pct` 還是僅暫存於頁面。
5. **不平衡率的公式與歸屬。** 見 D8。需確認 `|Δ| ÷ max` 是否為預期定義，並決定是否移入 `core`。
6. **「資料過期」橫幅的門檻。** 已拍板的 `stale_data_threshold_ms = 1000` 套在每列觀測上，輪詢型來源會永遠過期；本 change 沿用 `exchange-readonly-adapters` 的來源層級規則（`max(1000, 3 × 更新週期)`，「3 倍」為我的提議、未驗證）。需確認這個解讀是否符合使用者的意圖。
7. **對真實 demo 帳戶驗證時，持倉會全部「未配對」。** 是否接受用這個狀態通過 task 4.1，或需要先匯入／建立一組 `RECONCILED` 配對的測試資料。
8. **各所帳戶欄位到「資產列」與 initial margin 的對應**（D9）：Binance 的 initial margin 欄位與多資產模式、Bybit 的 `positionIM` 與 `usdValue`，全部未驗證。
9. **系統日誌「匯入」標示依 `legacy_hash`**：假設匯入器對所有匯入事件都寫入此欄（依 `legacy-event-import` spec 的規定），未對實際資料庫驗證。

## 未驗證項目清單

`gpui-component` 的 `PieChart` 與 `table` 在本機的可行性與效能；528 列在 1 秒 WebSocket 下的幀時間；更新合併頻率 2 Hz；日誌分頁大小 500；帳戶輪詢 30 秒與 Bybit、OKX 輪詢 10 秒（沿用 Python 版，未驗證其限流餘裕）；各所帳戶欄位對資產列與 initial margin 的對應；不平衡率公式；來源過期的「3 倍」係數；`legacy_hash` 在匯入事件上的實際填寫；Figma 的版面在 GPUI 下的可還原程度。

## 決定紀錄（2026-10-05，使用者）

- **費率與門檻的輸入**：在 `ui-trading-pages` 的風控頁完成前，以開發用子命令（寫入 store 的設定表）輸入；Net Edge 在設定前顯示「未設定」。
- **「加入交易單」欄與 Candidate List**：屬於會改變狀態的操作，歸入 `ui-trading-pages`（交易單頁），本 change 不做。
- 其餘 agent 提出的解讀（OKX 僅比價、日誌「FEED_RECOVERED」獨立事件、總資產不含名目本金等）：使用者無異議。

## 決定紀錄（2026-10-05 晚，使用者）

- **OKX 不提示**：維持只比價（Open Question 3）。
- **掃幣頁的 Net Edge 門檻唯讀**，到風控頁修改（Open Question 4）。
- 不平衡率：`engine-simulation` 已定為幣本位 `|Δ| ÷ max`（Open Question 5）。
