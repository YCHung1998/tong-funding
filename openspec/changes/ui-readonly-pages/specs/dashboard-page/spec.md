## ADDED Requirements

### Requirement: 總覽頁頂部彙總卡

總覽頁頂部 SHALL 顯示四張彙總卡：Total Portfolio Value（總資產，USDT）、每個已連線且支援帳戶資料的交易所各一張 Value（金額與「占總資產百分比」，百分比顯示到小數 2 位）、Open Positions（持倉腿數，並註明「N 對避險組合 · M 個交易所倉位」）。
總資產 SHALL 等於各交易所 Value 之和；各交易所百分比 SHALL 以同一個分母（總資產）計算。
標題列 SHALL 顯示「帳戶刷新 Ns」，N 為距離下次帳戶輪詢的剩餘秒數。

#### Scenario: 兩所的占比

- **WHEN** Binance 總值為 22,000.00 USDT、Bybit 為 18,000.00 USDT
- **THEN** 總資產顯示 40,000.00，兩張卡的百分比依序為 55.00% 與 45.00%

#### Scenario: 持倉與配對數

- **WHEN** 共有 4 個交易所倉位，其中 2 對被 `position-grouping` 配成組
- **THEN** Open Positions 顯示 4，並註明「2 對避險組合 · 4 個交易所倉位」

#### Scenario: 尚無任何配對

- **WHEN** 有 4 個交易所倉位但沒有任何 `RECONCILED` 配對
- **THEN** 註明「0 對避險組合 · 4 個交易所倉位」，且 4 個倉位被標示為未配對，不得顯示為「已避險」

### Requirement: 總資產的估值規則不得重複計入名目本金

每個交易所的 Value SHALL 為其資產明細各列的 USDT 價值之和，各列 SHALL 為：錢包中各幣種的餘額（以 USDT 估值），以及「合約權益」（已用保證金 + 可用保證金，兩者之和）。
持倉的名目本金 SHALL NOT 計入任何 Value（名目本金不是資產）。
非 USDT 幣種 SHALL 以交易所提供的 USDT 估值，或該交易所的 mark price 乘以數量估值；兩者皆取不到時，該列 SHALL 標示「無法估值」、不計入 Value，且頁面 SHALL 顯示「不含 N 項無法估值的資產」，SHALL NOT 以 0 或以數量直接當作 USDT 代替。
明細表底部 SHALL 有註腳說明「合約權益含已用與可用保證金，未重複計入合約名目本金」。

#### Scenario: 以 Figma 的數字驗算

- **WHEN** Binance 的 USDT 為 16,200、BTC（0.05 × 60,200）為 3,010、ETH（0.5 × 2,980）為 1,490，合約權益為 1,300（已用 900 + 可用 400），並有名目本金 2,400 的持倉
- **THEN** Binance Value 為 22,000.00（16,200 + 3,010 + 1,490 + 1,300），持倉名目本金不被加入

#### Scenario: 無法估值的資產

- **WHEN** 餘額中有某幣種且交易所沒有提供 USDT 估值、也沒有對應的 mark price
- **THEN** 該列標示「無法估值」，Value 不含它，頁面顯示「不含 1 項無法估值的資產」

### Requirement: 各交易所卡片的內容

每個支援帳戶資料的交易所 SHALL 有一張卡片，包含：交易所名稱與連線狀態標籤（例如「● CONNECTED · DEMO」，環境名稱為 DEMO 或 TESTNET，SHALL NOT 出現 PAPER 或 LIVE）、EXCHANGE TOTAL 與占總資產百分比、資產價值分布圖、持倉保證金分布圖、資產明細表。
資產價值分布圖 SHALL 依資產明細各列的 USDT 價值占該所 Value 的比例繪製，圖例列出各列名稱。
持倉保證金分布圖 SHALL 依各持倉的 initial margin 占「已用保證金總額」的比例繪製，並顯示已用保證金總額（USDT）。
持倉的 initial margin SHALL 取交易所回報值；交易所沒有提供時，SHALL 以名目本金除以槓桿估算，並在該圖標示「估算」。
沒有任何持倉時，持倉保證金分布圖 SHALL 顯示「無持倉」，不得顯示空白圖或 0%。

#### Scenario: 保證金分布

- **WHEN** 某所有 BTC 持倉 initial margin 400、ETH 持倉 500
- **THEN** 圖中顯示已用 900 USDT，BTC 為 44.44%、ETH 為 55.56%

#### Scenario: 交易所沒有提供 initial margin

- **WHEN** 某持倉的名目本金為 1,200、槓桿為 3，且交易所沒有回報 initial margin
- **THEN** 該持倉的保證金為 400，圖上標示「估算」

#### Scenario: 無持倉

- **WHEN** 某所沒有任何持倉
- **THEN** 持倉保證金分布圖位置顯示「無持倉」

### Requirement: 資產明細表的欄位與合計列

資產明細表 SHALL 有欄位：Asset、Price、Quantity、Value (USDT)、% of Exchange，最後一列為「合計」，顯示該所總值與 100.0000%。
% of Exchange SHALL 顯示到小數 4 位，且各列百分比的和 SHALL 與合計列一致（以同一分母計算，不各自四捨五入後再加總）。
USDT 列的 Price SHALL 為 1.00。

#### Scenario: 百分比以同一分母計算

- **WHEN** 某所 Value 為 22,000，各列 USDT 價值為 16,200、3,010、1,490、1,300
- **THEN** % of Exchange 依序顯示 73.6364%、13.6818%、6.7727%、5.9091%

### Requirement: 曝險摘要

總覽頁底部 SHALL 顯示曝險摘要一行，包含：依標的列出的中性組合數量、已用保證金總額、未實現 PnL 總額（皆為 USDT，取交易所回報值之和）、未避險單腿數量。
未避險單腿數量 SHALL 取自 `position-grouping` 的未配對持倉列數；大於 0 時 SHALL 以警示色顯示「N 個未避險單腿」，等於 0 時顯示「無未避險單腿」。

#### Scenario: 全部配對

- **WHEN** BTC 與 ETH 各有一組配對，已用保證金總額 1,800、未實現 PnL 合計 0，沒有未配對持倉
- **THEN** 顯示「BTC 與 ETH 各 1 對中性組合 · 已用保證金 1,800.00 USDT · 未實現 PnL 0.00 USDT · 無未避險單腿」

#### Scenario: 有單腿

- **WHEN** 有 1 列持倉無法配對
- **THEN** 摘要顯示警示色的「1 個未避險單腿」

### Requirement: 未連線與僅比價的交易所不得被當成零

交易所為「未連線」（金鑰缺失、時鐘未校時、請求失敗）時，其卡片 SHALL 顯示未連線與原因，其 Value SHALL NOT 計入總資產，且總資產卡 SHALL 註明「不含未連線的交易所：<名稱>」。
OKX（僅公開行情）SHALL 以一張說明卡呈現「僅比價，不提供帳戶資料」，SHALL NOT 顯示任何金額，也 SHALL NOT 計入總資產與占比。
資料取得失敗時，頁面 SHALL 保留最後一次成功的資料並標示其年齡與「資料可能已過期」，SHALL NOT 在失敗時把金額清為 0。

#### Scenario: 一所未連線

- **WHEN** Bybit 的金鑰不存在
- **THEN** Bybit 卡片顯示「未連線」與原因，總資產等於 Binance Value，並註明「不含未連線的交易所：Bybit」，Binance 的占比為 100.00%

#### Scenario: OKX 說明卡

- **WHEN** 開啟總覽頁
- **THEN** OKX 位置為「僅比價，不提供帳戶資料」說明卡，沒有金額，總資產不含 OKX

#### Scenario: 輪詢失敗保留舊資料

- **WHEN** 帳戶輪詢失敗，上一次成功在 45 秒前
- **THEN** 頁面仍顯示當時的金額，並標示「45 秒前的資料 · 可能已過期」，金額不被改為 0
