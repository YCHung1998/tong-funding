## ADDED Requirements

### Requirement: OKX 是可下單交易所

具備下單能力的交易所集合 SHALL 為 Binance、Bybit 與 OKX；是否實際交易某所 SHALL 只由風控設定的 `allowed_exchanges` 決定。
掃幣頁的達標判定、Gross Spread、Net Edge 與「最佳套利方向」SHALL 在 `allowed_exchanges` 內的交易所之間計算，包含 OKX；OKX 儲存格 SHALL NOT 再帶「僅比價」標示。
本 requirement 取代 `scanner-page`「OKX 僅供比價，不參與達標與方向」與 `scanner-candidates`「含 OKX 的列不可勾選」。

#### Scenario: OKX 價差最大時被選為方向

- **WHEN** 某標的 OKX 與 Binance 的 rate 差最大且達標，`allowed_exchanges` 含三所
- **THEN** 最佳套利方向為 OKX 與 Binance 之間的配對，該列可勾選

#### Scenario: 使用者排除 OKX

- **WHEN** `allowed_exchanges` 只含 Binance 與 Bybit
- **THEN** OKX 儲存格仍顯示 rate 與週期，但不參與方向與達標

### Requirement: 手動下單的 OKX 面板以幣量輸入

手動下單頁 SHALL 提供 OKX 面板。使用者輸入的數量 SHALL 為幣量，系統 SHALL 以該標的 `ctVal` 換算為張數並依 `lotSz` / `minSz`（張數）無條件捨去，顯示「N 張（≈ x 幣，≈ y USDT）」，送出的數量 SHALL 為該張數。
`ctVal` 或合約規格未知時，送出 SHALL 被停用並顯示原因；換算後低於最小張數時 SHALL 顯示「低於最小下單量」。
從持倉選擇器帶入的 OKX 持倉 SHALL 以張數 × `ctVal` 的幣量填入。
本 requirement 取代 `manual-order-page`「頁面 SHALL 只提供 Binance 與 Bybit 兩個交易所的面板」。

#### Scenario: 幣量換算為張數

- **WHEN** 使用者在 OKX 面板輸入 `BTCUSDT` 數量 0.037，`ctVal` 為 0.01、`lotSz` 為 1，價格 60,000
- **THEN** 顯示「3 張（≈ 0.03 BTC，≈ 1800 USDT）」，送出的訂單數量為 3

#### Scenario: 缺合約面值

- **WHEN** OKX 該標的 `ctVal` 未知
- **THEN** 送出按鈕停用，原因為「無法取得合約規格（ctVal）」

### Requirement: OKX 數量在比較與顯示前換算為幣量

頁面上任何來自 OKX 的數量（持倉、未成交委託、成交量、最佳一檔掛單量）SHALL 先乘以該標的 `ctVal` 再與幣量比較或以幣量顯示；`ctVal` 未知時 SHALL 標示「無法換算」，SHALL NOT 以張數當作幣量。
交易單頁判斷「會吃到第二檔」時，OKX 的一檔掛單量 SHALL 以張數 × `ctVal` 與共同數量比較；`ctVal` 未知時 SHALL 顯示「無法判斷」而非提示或不提示。

#### Scenario: 一檔掛單量以幣量比較

- **WHEN** 共同數量 0.016 BTC，OKX 賣一量 1 張、`ctVal` 0.01
- **THEN** 多腿顯示「會吃到第二檔」（0.01 BTC < 0.016 BTC）

#### Scenario: 持倉頁的 OKX 列

- **WHEN** OKX 回報 `BTCUSDT` 空單 3 張、`ctVal` 0.01
- **THEN** 持倉頁顯示 −0.03 BTC（3 張），並與另一所的 0.03 BTC 多單歸為同一組

### Requirement: 總覽與持倉頁呈現 OKX 帳戶資料

總覽 SHALL 以與 Binance、Bybit 相同規則呈現 OKX 帳戶卡：資產列取餘額明細的幣別、數量與交易所估值，合約權益的已用為 OKX 持倉初始保證金之和、可用為 OKX 可用保證金；OKX 未連線、讀取失敗、載入中或帳戶模式不支援時 SHALL 以對應狀態卡呈現附原因，SHALL NOT 計入總資產與占比，SHALL NOT 顯示為 0。
持倉頁 SHALL 列出 OKX 持倉，SHALL NOT 再顯示「OKX 僅比價，不顯示持倉」；OKX 持倉列表不完整、未連線或讀取失敗時 SHALL 以既有的提示規則顯示。
本 requirement 取代 `dashboard-page` 的 OKX 說明卡與 `positions-page` 的 OKX 註記。

#### Scenario: OKX 已連線

- **WHEN** OKX 帳戶讀取成功，USDT `eq` 為 5000、`eqUsd` 為 4999.8
- **THEN** 總覽出現 OKX 帳戶卡，價值計入總資產與占比

#### Scenario: OKX 帳戶模式不支援

- **WHEN** OKX 帳戶的 `posMode` 為 `long_short_mode`
- **THEN** 總覽 OKX 卡顯示「帳戶模式不支援（long_short_mode）」，總資產不含 OKX，持倉頁顯示同一原因的提示
