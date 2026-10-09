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
- **THEN** 未啟用的交易所不抓行情，OKX 儲存格顯示「—」，不參與方向與達標

### Requirement: 手動下單的 OKX 面板以幣量輸入

手動下單頁 SHALL 提供 OKX 面板。使用者輸入的數量 SHALL 為幣量，系統 SHALL 以該標的 `ctVal` 換算為張數並依 `lotSz` / `minSz`（張數）無條件捨去，顯示「N 張（≈ x 幣，≈ y USDT）」，送出的數量 SHALL 為該張數。
`ctVal` 或合約規格未知時，送出 SHALL 被停用並顯示原因；換算後低於最小張數時 SHALL 顯示「低於最小下單量」。
從持倉選擇器帶入的 OKX 持倉 SHALL 以張數 × `ctVal` 的幣量填入。
本 requirement 取代 `manual-order-page`「頁面 SHALL 只提供 Binance 與 Bybit 兩個交易所的面板」。

#### Scenario: 幣量換算為張數

- **WHEN** 使用者在 OKX 面板輸入 `BTCUSDT` 數量 0.037，`ctVal` 為 0.01、`lotSz` 為 1，價格 60,000
- **THEN** 顯示「3 張（≈ 0.03 BTC，≈ 1,800.00 USDT）」，送出的訂單數量為 3

#### Scenario: 缺合約面值

- **WHEN** OKX 該標的 `ctVal` 未知
- **THEN** 送出按鈕停用，原因為「無法取得合約規格（ctVal）」

### Requirement: OKX 數量在比較與顯示前換算為幣量

頁面上任何來自 OKX 的數量（持倉、未成交委託、成交量、最佳一檔掛單量）SHALL 先乘以該標的 `ctVal` 再與幣量比較或以幣量顯示；`ctVal` 未知時 SHALL 標示「無法換算」，SHALL NOT 以張數當作幣量。
交易單頁判斷「會吃到第二檔」時，OKX 的一檔掛單量 SHALL 以張數 × `ctVal` 與共同數量比較；`ctVal` 未知時 SHALL 因無共同數量而不顯示成本預估（原因含 `ctVal`），而非提示或不提示。

#### Scenario: 一檔掛單量以幣量比較

- **WHEN** 共同數量 0.016 BTC，OKX 賣一量 1 張、`ctVal` 0.01
- **THEN** 多腿顯示「會吃到第二檔」（0.01 BTC < 0.016 BTC）

#### Scenario: 持倉頁的 OKX 列

- **WHEN** OKX 回報 `BTCUSDT` 空單 3 張、`ctVal` 0.01
- **THEN** 持倉頁顯示 −0.03 BTC（3 張），並與另一所的 0.03 BTC 多單歸為同一組

### Requirement: 總覽與持倉頁呈現 OKX 帳戶資料

總覽 SHALL 以與 Binance、Bybit 相同規則呈現 OKX 帳戶卡：資產列取餘額明細的幣別、數量與交易所估值，合約權益列暫不顯示（與 Binance、Bybit 相同：欄位對應未驗證，且錢包資產已含保證金，相加會重複計入），已用保證金由持倉 `imr` 顯示於保證金分佈；OKX 未連線、讀取失敗、載入中或帳戶模式不支援時 SHALL 以對應狀態卡呈現附原因，SHALL NOT 計入總資產與占比，SHALL NOT 顯示為 0。
持倉頁 SHALL 列出 OKX 持倉，SHALL NOT 再顯示「OKX 僅比價，不顯示持倉」；OKX 持倉列表不完整、未連線或讀取失敗時 SHALL 以既有的提示規則顯示。
本 requirement 取代 `dashboard-page` 的 OKX 說明卡與 `positions-page` 的 OKX 註記。

#### Scenario: OKX 已連線

- **WHEN** OKX 帳戶讀取成功，USDT `eq` 為 5000、`eqUsd` 為 4999.8
- **THEN** 總覽出現 OKX 帳戶卡，價值計入總資產與占比

#### Scenario: OKX 帳戶模式不支援

- **WHEN** OKX 帳戶的 `posMode` 為 `long_short_mode`
- **THEN** 總覽 OKX 卡顯示「帳戶模式不支援（long_short_mode）」，總資產不含 OKX，持倉頁顯示同一原因的提示

### Requirement: OKX 下單前套用槓桿並共用閂鎖

OKX 開倉單帶 `leverage` 時，executor SHALL 在帳戶模式讀取（示範金鑰證明）與數量守衛通過之後、下單之前，以經旗標建構器簽名的 `POST /api/v5/account/set-leverage`（cross、net 模式、不帶 `posSide`）套用；任何非成功回覆 SHALL 使訂單不送出（`not_sent`）。平倉與未帶槓桿的開倉 SHALL NOT 送出此請求。OKX 每標的最大槓桿 SHALL 讀自 instruments 的 `lever`（標的上限，非依倉位大小分級）。
閂鎖 SHALL 只有一個 `Arc<OkxLatch>`（由執行器工廠擁有），OKX 讀取客戶端 SHALL 共用它；其原因 SHALL 以不可關閉的停機橫幅顯示。OKX 開倉的單腿名目上限 SHALL 為合約範本 notional 的 2 倍，範本不可讀時為未知（開倉不送）。OKX 帳戶未連線 SHALL NOT 產生帳戶斷線警示（金鑰為選用）。

#### Scenario: 設槓桿後才下單

- **WHEN** OKX 開倉單帶 leverage 5
- **THEN** 請求順序為 account/config、set-leverage、trade/order，且三者皆帶 `x-simulated-trading: 1`

#### Scenario: 設槓桿失敗

- **WHEN** set-leverage 被拒絕、限流、逾時或結果不明
- **THEN** 訂單不送出，原因含 leverage

#### Scenario: 無持倉可平的 OKX 平倉

- **WHEN** OKX 平倉回 51169（無持倉可減）
- **THEN** 結果維持「不明」（拒絕白名單不變），原因文字說明可能已是空倉而非斷言對腿裸露

### Requirement: 開倉前先檢查並設定兩腿，才送任何訂單

配對的兩個開倉腿 SHALL 在送出任何一腿之前通過 executor 的送前屏障：兩腿的本地檢查（OKX 數量守衛、limits 與標記價）、時鐘與模式檢查，以及**兩腿**的槓桿設定；任一失敗 SHALL 使兩腿都不送出，配對以原因取消。OKX 腿的名目上限 SHALL 來自該配對自己的進場 notional。OKX 缺 `ctVal` SHALL 為錯誤。OKX 在 `allowed_exchanges` 內時，其未連線與時鐘警示、以及切換 EXCHANGE_DEMO 所需的金鑰 SHALL 與另兩所相同。

#### Scenario: 一所拒絕槓桿

- **WHEN** OKX 或 Binance 的 set-leverage 被拒絕
- **THEN** 兩所都不送單，配對取消，原因含 `nothing sent` 與 leverage

#### Scenario: 合約範本事後調低

- **WHEN** 配對加入後合約範本 notional 被調低
- **THEN** OKX 腿的上限仍依該配對自己的 notional，配對不因此被擋

#### Scenario: 屏障通過後的暫時性失敗

- **WHEN** 兩腿通過送前屏障後，OKX 模式快取過期且重讀失敗（或 back-off 開始、limits 消失）
- **THEN** 兩腿仍都送出（已核准的腿不再重做軟性檢查）；僅閂鎖與示範金鑰證明這類硬性停止仍可擋下，並在原因中點名對側可能為裸腿

#### Scenario: 屏障進行中的視窗

- **WHEN** 屏障進行中 kill switch 被按下、進場視窗結束、或屏障逾時
- **THEN** 不送任何一腿，配對以 nothing sent 取消；同交易所同標的的手動開倉單在此期間被拒絕

#### Scenario: 兩腿意圖原子落地

- **WHEN** 配對的兩腿通過屏障準備送出，而資料庫在兩腿意圖落地之間（或之後）停機
- **THEN** 要麼兩腿意圖都已落地且兩腿都送出，要麼兩腿都不送（落地失敗會停機，配對停在 ORDER_SUBMIT 待重啟後恢復）；不會只送一腿

#### Scenario: 同標的的兩個配對同時進場

- **WHEN** 一個配對處於 PRE_TRADE_CHECK 或 ORDER_SUBMIT，另一個共用交易所與標的的配對（或手動帶槓桿開倉）嘗試進場
- **THEN** 後者被拒絕；只有先到的配對進入屏障
