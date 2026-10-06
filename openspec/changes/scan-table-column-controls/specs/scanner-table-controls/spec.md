## ADDED Requirements

### Requirement: 欄位顯示與隱藏

掃幣表上方 SHALL 有一排欄位按鈕，每個欄位一顆（Rank、Symbol、覆蓋、結算倒數、Binance、Bybit、OKX、最佳套利方向、Gross Spread %、Net Edge %、達標、加入交易單）。點按鈕 SHALL 切換該欄的顯示與隱藏，且按鈕 SHALL 以可辨識的樣式標示目前狀態（顯示中與已隱藏）。
Symbol 欄 SHALL 固定顯示，其按鈕 SHALL 不可切換。
頁面 SHALL 提供「重設欄位」，一次恢復全部欄位顯示。
隱藏欄位 SHALL 只影響顯示，SHALL NOT 影響達標判定、排序依據、Candidate List 與其他頁面。

#### Scenario: 隱藏一個欄位

- **WHEN** 使用者點「OKX」按鈕
- **THEN** 表格不再顯示 OKX 欄，其餘欄位順序不變，OKX 按鈕呈現已隱藏樣式

#### Scenario: 再點一次恢復

- **WHEN** OKX 已隱藏，使用者再點「OKX」按鈕
- **THEN** OKX 欄恢復顯示在原本的位置

#### Scenario: Symbol 不可隱藏

- **WHEN** 使用者點 Symbol 按鈕
- **THEN** 沒有任何變化，Symbol 欄仍顯示

#### Scenario: 重設欄位

- **WHEN** 已隱藏三個欄位，使用者點「重設欄位」
- **THEN** 十二欄全部顯示

#### Scenario: 隱藏的欄位不改變達標

- **WHEN** 隱藏「Net Edge %」欄
- **THEN** 達標欄的內容與「只顯示達標」的結果與隱藏前完全相同

### Requirement: 橫向捲動與固定左側欄

欄位總寬超過表格可視寬度時，表格 SHALL 可橫向捲動（滾輪或觸控板水平手勢、或橫向捲軸），標題列 SHALL 與資料列同步捲動。
Rank 與 Symbol 欄 SHALL 固定在表格左側（Rank 被隱藏時只固定 Symbol），橫向捲動時 SHALL 保持可見。
隱藏欄位後總寬縮短，若不再超過可視寬度 SHALL 不出現橫向捲動。

#### Scenario: 窄視窗可以看到右側欄位

- **WHEN** 視窗寬度小於十二欄的總寬，使用者向右捲動
- **THEN** 能看到最右側的「加入交易單」欄，且 Rank 與 Symbol 欄仍在左側

#### Scenario: 標題與內容同步

- **WHEN** 使用者向右捲動 300 px
- **THEN** 標題列與資料列的欄位對齊沒有錯位

#### Scenario: 隱藏後不需要捲動

- **WHEN** 隱藏足夠的欄位使總寬小於可視寬度
- **THEN** 沒有橫向捲動，所有欄位都在視窗內

### Requirement: 點欄位標題排序

可排序欄位為：Rank、Symbol、覆蓋、結算倒數、Binance、Bybit、OKX、Gross Spread %、Net Edge %、達標。「最佳套利方向」與「加入交易單」SHALL NOT 可排序。
點可排序欄位的標題 SHALL 依序循環：降冪、升冪、預設排序（再點回到未排序）。同一時間 SHALL 只有一個欄位有排序狀態；點另一欄時，前一欄回到未排序。
標題 SHALL 顯示目前排序方向的圖示。
預設排序 SHALL 與 `scanner-page` 定義的預設排序（Net Edge 由高到低，依規則處理無 Net Edge 與設定不完整的情況）完全一致。

#### Scenario: 三態循環

- **WHEN** 預設排序下，連續點三次「Net Edge %」標題
- **THEN** 第一次依 Net Edge 由高到低、第二次由低到高、第三次回到預設排序

#### Scenario: 換欄位

- **WHEN** 已依「Gross Spread %」降冪排序，使用者點「Symbol」標題
- **THEN** 列改依 Symbol 排序，Gross Spread % 標題回到未排序圖示

#### Scenario: 不可排序的欄位

- **WHEN** 使用者點「加入交易單」標題
- **THEN** 排序狀態不變

### Requirement: 排序鍵與缺值處理

各欄位的排序鍵 SHALL 為：Rank＝預設排序名次；Symbol＝字串（不分大小寫）；覆蓋＝有 `LISTED` 觀測的交易所數，再依已啟用交易所數；結算倒數＝結算目標時間（越近越小）；Binance、Bybit、OKX＝該所的 funding rate 數值；Gross Spread %、Net Edge %＝其數值；達標＝達標者在前（降冪）或在後（升冪），「未設定」與未達標視為同一組。
沒有值的列（rate 為「—」或「資料異常」、沒有 Gross Spread、沒有 Net Edge、沒有結算目標）SHALL 不論升冪或降冪都排在最後。
排序鍵相同時 SHALL 依預設排序名次由小到大（穩定排序），使相同排序鍵的列順序不會在每次刷新時跳動。
Rank 欄 SHALL 仍顯示預設排序下的名次，SHALL NOT 因使用者排序而重新編號。
結算倒數欄的排序 SHALL 依結算目標時間而非即時剩餘秒數，因此每秒倒數更新 SHALL NOT 導致列順序變動。

#### Scenario: 缺值永遠在最後

- **WHEN** 三列的 Net Edge 為 0.03、「—」、−0.02，依 Net Edge 升冪
- **THEN** 順序為 −0.02、0.03、「—」；改為降冪時為 0.03、−0.02、「—」

#### Scenario: Rank 不重新編號

- **WHEN** 依 Symbol 升冪排序
- **THEN** 每列的 Rank 仍是預設排序下的名次（例如第一列可能是 Rank 17）

#### Scenario: 相同鍵的穩定順序

- **WHEN** 兩列的 Binance rate 相同，依 Binance 降冪排序
- **THEN** 預設名次較小的列在前，行情刷新後兩列的順序不變

#### Scenario: 倒數更新不改順序

- **WHEN** 依結算倒數排序，經過 1 秒沒有新資料
- **THEN** 列順序不變，只有倒數文字更新

#### Scenario: 達標欄排序

- **WHEN** 依達標欄降冪排序
- **THEN** 達標的列在前，「—」與「未設定」的列在後

### Requirement: 狀態在刷新與篩選後保留

欄位顯示狀態與排序狀態 SHALL 在行情更新、「立即刷新」、切換「只顯示達標」、切換頁面再回來時保留。
排序 SHALL 套用在目前顯示的列集合上（含「只顯示達標」的篩選結果）；「加入交易單」儲存格 SHALL 與排序後的列對齊（點哪一列就加入哪個標的）。
排序與欄位狀態 SHALL 只存在於本次執行期間，不寫入資料庫或設定檔（v1）。

#### Scenario: 刷新後仍是同一排序

- **WHEN** 已依 Gross Spread 降冪排序，行情更新兩次
- **THEN** 列仍依 Gross Spread 降冪排序，標題仍顯示降冪圖示

#### Scenario: 與只顯示達標並用

- **WHEN** 開啟「只顯示達標」並依 Symbol 升冪
- **THEN** 只顯示達標的列，且依 Symbol 升冪

#### Scenario: 加入交易單點到正確的列

- **WHEN** 依 Symbol 排序後點第一列的「加入」
- **THEN** 被加入的是該列的標的，而不是預設排序下第一列的標的

#### Scenario: 切頁再回來

- **WHEN** 隱藏 OKX、依 Net Edge 升冪後切到持倉頁再回到掃幣頁
- **THEN** OKX 仍隱藏、仍依 Net Edge 升冪

### Requirement: 排序不得拖慢表格

排序 SHALL 只在表格重算時（受 `scanner-page` 的頻率上限約束）或使用者點標題時執行，SHALL NOT 在每次繪製、每秒倒數更新時執行。
528 列排序 SHALL 在單次不超過 5 ms 內完成（開發機量測值記錄於 TODO.md 的效能待量項目），並且 SHALL NOT 使 `bootstrap-gpui-shell` 記錄的幀時間預算惡化。

#### Scenario: 倒數更新不排序

- **WHEN** 只有時間經過 1 秒
- **THEN** 排序函式沒有被呼叫

#### Scenario: 528 列的排序時間

- **WHEN** 對 528 列依任一可排序欄位排序
- **THEN** 單次排序時間不超過 5 ms
