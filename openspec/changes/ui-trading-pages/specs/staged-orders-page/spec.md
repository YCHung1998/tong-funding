## ADDED Requirements

### Requirement: 交易單清單逐筆顯示雙腿內容

交易單頁 SHALL 列出所有狀態為 `PREPARED` 的暫存配對，每一筆 SHALL 顯示：勾選框、標的、Gross Spread、Net Edge、long 腿與 short 腿各自的交易所與數量、每腿 Notional、每腿 Margin、槓桿，以及「移除」操作。
數量 SHALL 為依 `quantity-precision` 向下取整後的數量與單位，SHALL NOT 顯示未取整的試算值。
頁首 SHALL 顯示暫存筆數與已選筆數，並提供「全選」與「全不選」。
頁首的模板摘要 SHALL 顯示目前的 `execution_mode`（`SIMULATION` 或 `EXCHANGE_DEMO`）與下單型別，SHALL NOT 寫死模式字樣。
取整後低於最小下單量的配對 SHALL 顯示「低於最小下單量」，其勾選框 SHALL 被禁用，且 SHALL NOT 被「全選」選取。
配對若為 `PREPARED` 且有進場時間，SHALL 顯示進場倒數。

#### Scenario: 全選只選取可送出的配對

- **WHEN** 清單有 3 筆暫存配對，其中 1 筆取整後低於最小下單量，使用者按「全選」
- **THEN** 已選為 2 筆，低於最小下單量那一筆維持未選取且勾選框禁用

#### Scenario: 數量顯示取整後的值

- **WHEN** 某配對每腿 Notional 為 1,200 USDT、現價 60,200、`step_size` 為 0.001
- **THEN** 該腿數量顯示為 0.019 BTC，而不是 0.019934 BTC

#### Scenario: 模式字樣隨設定變化

- **WHEN** `execution_mode` 由 `SIMULATION` 改為 `EXCHANGE_DEMO`
- **THEN** 模板摘要即時改顯示 `EXCHANGE_DEMO`

### Requirement: 選取摘要顯示腿數、總額與可用保證金

頁面 SHALL 在清單下方顯示已選配對數、總腿數（每筆配對 2 腿）、總 Notional、總 Margin，以及 Binance 與 Bybit 各自最近一次取得的可用保證金與其取得時間。
可用保證金 SHALL 取自交易所帳戶查詢結果，SHALL NOT 顯示任何虛構的「Paper cash」。
可用保證金查詢失敗或過期時 SHALL 顯示「未知」與原因，SHALL NOT 以 0 或上次舊值冒充。

#### Scenario: 摘要手算一致

- **WHEN** 已選 2 筆配對，每腿 Notional 皆 1,200 USDT、槓桿皆 3×
- **THEN** 摘要顯示 4 腿、總 Notional 4,800.00、總 Margin 1,600.00

#### Scenario: 可用保證金未知

- **WHEN** Bybit 的餘額查詢失敗
- **THEN** Bybit 的可用保證金顯示「未知」與失敗原因，而不是 0

### Requirement: 一鍵送出前須逐腿列出並二次確認

按下「一鍵送出已選取」SHALL 先開啟確認視窗，視窗 SHALL 列出將送出的每一腿：交易所、標的、方向（BUY 或 SELL）、取整後的數量字串、Notional、槓桿、Margin。
視窗 SHALL 同時顯示本次的 `execution_mode` 與目標環境（`SIMULATION` 時註明「不會送出真實訂單」；`EXCHANGE_DEMO` 時註明「將對 demo / testnet 帳戶真實下單」）。
只有使用者在確認視窗按下確認後，頁面 SHALL 才向 engine 送出一個執行命令；取消或關閉視窗 SHALL NOT 送出任何命令。
頁面 SHALL 僅以 engine 的 Command 送單，SHALL NOT 直接呼叫任何交易所。

#### Scenario: 確認視窗列出每一腿

- **WHEN** 已選 2 筆配對後按「一鍵送出」
- **THEN** 確認視窗列出 4 腿，每腿皆有交易所、標的、方向、數量、Notional、槓桿與 Margin

#### Scenario: 取消確認不送單

- **WHEN** 確認視窗開啟後使用者按取消
- **THEN** engine 收到 0 個命令，已選狀態保持不變

#### Scenario: 確認後恰好送出一個命令

- **WHEN** 使用者在確認視窗按下確認
- **THEN** engine 恰好收到一個包含所選配對的執行命令

#### Scenario: EXCHANGE_DEMO 的環境警示

- **WHEN** `execution_mode` 為 `EXCHANGE_DEMO` 時開啟確認視窗
- **THEN** 視窗顯示「將對 demo / testnet 帳戶真實下單」

### Requirement: 設定不完整或不可執行時禁用送出並說明原因

下列任一情況成立時，「一鍵送出」SHALL 被禁用，且頁面 SHALL 在按鈕旁顯示具體原因：未選取任何配對；風控設定不完整（SHALL 列出缺漏的欄位名稱，例如某所 `taker_fee_pct`）；kill switch 已啟動；系統處於停機（失敗即封閉）狀態；所選配對已不是 `PREPARED`。
禁用 SHALL 只依狀態判定，不得依賴使用者重新整理頁面。

#### Scenario: 缺少費率

- **WHEN** Bybit 的 `taker_fee_pct` 尚未填寫
- **THEN** 「一鍵送出」被禁用，原因顯示「設定不完整：Bybit taker_fee_pct」

#### Scenario: kill switch 已啟動

- **WHEN** kill switch 為啟動狀態
- **THEN** 「一鍵送出」被禁用，原因顯示「緊急停止中」

#### Scenario: 未選取任何配對

- **WHEN** 已選筆數為 0
- **THEN** 「一鍵送出」被禁用，原因顯示「尚未選取配對」

#### Scenario: 配對在確認期間被排程器取走

- **WHEN** 所選某配對在使用者開啟確認視窗後已被 engine 轉出 `PREPARED`
- **THEN** 確認視窗標示該配對已不可送出並將其排除，確認後的命令不含該配對

### Requirement: 送單前檢查的結果以 engine 回報為準並完整呈現

頁面 SHALL NOT 自行判定送單前檢查是否通過。
engine 回報某配對 BLOCK 時，頁面 SHALL 列出該配對所有未通過的檢查名稱（例如 `PriceDrift`、`Margin`），SHALL NOT 只顯示第一項。
被 BLOCK 的配對 SHALL 顯示為未送出，且 SHALL NOT 出現在持倉。

#### Scenario: 多項檢查同時失敗

- **WHEN** engine 回報某配對 BLOCK，未通過清單為 `PriceDrift` 與 `Margin`
- **THEN** 頁面對該配對同時顯示 `PriceDrift` 與 `Margin`

### Requirement: 上次執行結果如實標示模式與每腿狀態

「上次執行結果」區塊 SHALL 同時涵蓋 `SIMULATION` 與 `EXCHANGE_DEMO` 兩種模式，並 SHALL 如實標示該次執行當時的模式；結果 SHALL 取自不可變事件，使重啟後仍可顯示。
每一腿 SHALL 顯示交易所、方向、狀態（成功、失敗、未送出）與 order id；`SIMULATION` 的結果 SHALL 標示為模擬且 order id SHALL NOT 以真實 order id 的樣式呈現。
任一腿失敗時，區塊 SHALL 明確顯示「需人工處理」及該配對目前的狀態，SHALL NOT 顯示「已平倉回滾」或任何暗示系統已自動補救的文字。
區塊 SHALL 標示結果為歷史紀錄，並非目前已選項目的送單結果。

#### Scenario: SIMULATION 結果

- **WHEN** 上一批在 `SIMULATION` 下執行
- **THEN** 區塊標題標示 `SIMULATION`，每腿標示為模擬，且不出現真實 order id

#### Scenario: EXCHANGE_DEMO 結果

- **WHEN** 上一批在 `EXCHANGE_DEMO` 下執行且兩腿成功
- **THEN** 區塊標示 `EXCHANGE_DEMO`，每腿顯示交易所回報的 order id

#### Scenario: 單腿失敗不得宣稱已回滾

- **WHEN** 上一批 Binance 腿成功、Bybit 腿失敗，配對為 `PARTIAL_FAILURE`
- **THEN** 區塊顯示「需人工處理」與狀態 `PARTIAL_FAILURE`，且不出現「已平倉回滾」

#### Scenario: 重啟後仍可顯示

- **WHEN** 程式重啟後開啟交易單頁
- **THEN** 上次執行結果仍由事件還原並顯示

### Requirement: 單腿失敗與不平衡由人工處理並提供入口

對狀態為 `PARTIAL_FAILURE`、`IMBALANCED` 或 `UNRESOLVED` 的配對，頁面 SHALL 顯示該配對的每腿狀態與提供兩個人工操作：「人工要求平倉」與「人工確認已平倉」，分別對應 core 的人工要求平倉與人工確認已平倉事件。
頁面 SHALL NOT 提供或觸發任何自動補買、補賣或平倉的動作。
「人工要求平倉」SHALL 先顯示確認視窗，列出將以 reduce-only 平掉的每一腿與其數量（取自交易所回報的實際持倉），使用者確認後才送出命令。
「人工確認已平倉」SHALL 僅在最新一次向交易所查得「兩腿持倉皆為 0 且無未成交委託」時可用；查詢失敗或結果為非零時 SHALL 禁用並顯示原因。
全頁常駐橫幅與系統通知由 `alert-banner` 與 `partial-failure-alerting` 負責，本頁 SHALL 只提供橫幅所指向的處理入口。

#### Scenario: 人工要求平倉需確認

- **WHEN** 使用者對 `PARTIAL_FAILURE` 配對按「人工要求平倉」
- **THEN** 先顯示確認視窗列出每一腿與實際持倉數量，確認前 engine 收到 0 個命令

#### Scenario: 持倉未歸零不可確認已平倉

- **WHEN** 最新查詢顯示 Bybit 仍有 0.019 的持倉
- **THEN** 「人工確認已平倉」被禁用，原因顯示「Bybit 仍有持倉」

#### Scenario: 查詢失敗不可確認

- **WHEN** 查詢交易所持倉時失敗
- **THEN** 「人工確認已平倉」被禁用，原因顯示查詢失敗，而不是視為已平倉

#### Scenario: 頁面不提供自動補救

- **WHEN** 檢視任一 `PARTIAL_FAILURE` 配對的所有可用操作
- **THEN** 只有「人工要求平倉」與「人工確認已平倉」，沒有補買、補賣或自動平倉的操作

### Requirement: 觸發模式與進行中配對的平倉入口

交易單頁 SHALL 提供 `trigger_mode`（AUTO 或 MANUAL）切換，切換 SHALL 持久化並寫入 `TRIGGER_MODE_CHANGED` 事件；此開關 SHALL 與 `execution_mode` 互相獨立，改變其一 SHALL NOT 改變另一個。
狀態為 `RECONCILED` 的配對 SHALL 顯示平倉倒數；`trigger_mode` 為 MANUAL 時 SHALL 提供「立即平倉」（同樣先列出將平倉的每一腿並二次確認），AUTO 時顯示「自動」。

#### Scenario: 兩個開關互相獨立

- **WHEN** 使用者把 `trigger_mode` 由 AUTO 改為 MANUAL
- **THEN** `execution_mode` 維持原值，且寫入一筆 `TRIGGER_MODE_CHANGED` 事件

#### Scenario: MANUAL 才有立即平倉

- **WHEN** `trigger_mode` 為 AUTO 並檢視一筆 `RECONCILED` 配對
- **THEN** 該列顯示「自動」與平倉倒數，沒有「立即平倉」按鈕
