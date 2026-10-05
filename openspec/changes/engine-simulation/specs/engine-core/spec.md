## ADDED Requirements

### Requirement: 單一 actor 擁有全部可變狀態

引擎 SHALL 由單一 actor（一個 tokio task）擁有全部可變的交易狀態（配對、模式、kill switch 快取、排程狀態）。
UI 與其他模組 SHALL 只能透過 `Command` 變更狀態、只能透過 `Snapshot` 讀取狀態，SHALL NOT 持有可變狀態的共享參考（例如 `Arc<Mutex<…>>`）。
所有 `Command` 與內部 `Event` SHALL 在同一個迴圈內逐一處理，因此同一時間只有一個處理流程能修改狀態。

#### Scenario: 兩個 Command 同時送入

- **WHEN** 兩個 `Command`（對同一標的各送一次「新增 PREPARED 配對」）幾乎同時送入 actor
- **THEN** 依到達順序逐一處理，恰好一個成功、另一個收到「已有待處理」，且不需要任何額外的鎖

#### Scenario: 對外介面不暴露可變狀態

- **WHEN** 檢視 `engine` 模組的公開介面
- **THEN** 只有 `Command` 發送端、`Snapshot` 接收端與啟動函式，沒有任何回傳可變狀態參考的函式

### Requirement: Command 是否增加曝險必須窮舉分類

每個 `Command` 變體 SHALL 由 `Command::opens_exposure()` 明確分類為「增加曝險」或「不增加曝險」。
該函式的 `match` SHALL 逐一列出所有變體，SHALL NOT 含萬用分支（`_ =>`），使新增變體而未分類時無法通過編譯。
資料相依的變體（例如手動下單）SHALL 以其欄位決定分類（見 design D5），而不是整個變體一刀切。

#### Scenario: 新增變體必須分類

- **WHEN** 在 `Command` 新增一個變體但沒有修改 `opens_exposure()`
- **THEN** 編譯失敗（以編譯失敗測試或 lint 於 CI 驗證）

#### Scenario: 手動下單依是否 reduce-only 分類

- **WHEN** 檢查「手動下單」Command，一次 `reduce_only` 為真、一次為假
- **THEN** 前者 `opens_exposure()` 為假，後者為真

### Requirement: I/O 不得阻塞 actor 迴圈

下單、撤單、查單與其他交易所 I/O SHALL 由 actor spawn 的獨立 task 執行，結果 SHALL 以內部 `Event` 回送 actor；actor 迴圈 SHALL NOT `await` 任何交易所呼叫。
actor 在等待某個 I/O 結果期間，SHALL 持續處理 tick、Command 與其他配對的事件。

#### Scenario: 下單呼叫卡住不影響排程

- **WHEN** 假的 `Executor` 對某一腿的送單永遠不回應，同時注入時鐘前進 30 秒
- **THEN** actor 仍每秒處理一次 tick、其他配對的轉移不被延遲，該腿在逾時規則下進入明確狀態

#### Scenario: 結果回送後才改變狀態

- **WHEN** 送單 task 完成並回送 `Event`
- **THEN** 配對狀態只在 actor 處理該 `Event` 時才改變，spawn 的 task 本身不修改任何狀態

### Requirement: 狀態轉移只經由 core 的 next() 並先落地再生效

配對狀態的每次改變 SHALL 由 `core` 的 `next(狀態, 事件)` 決定；`next()` 回傳非法轉移錯誤時，引擎 SHALL 記錄事件並維持原狀態，SHALL NOT 自行改寫狀態。
每次合法轉移 SHALL 先成功寫入 `store`（`pairs` 狀態與一筆不可變事件於同一 transaction），寫入成功後才可執行該轉移附帶的動作（例如送單）。
寫入失敗 SHALL 使引擎進入停機並拒絕所有 `opens_exposure` 為真的 Command，SHALL NOT 在記憶體中先行改變狀態後繼續。

#### Scenario: 非法轉移被拒絕並留下事件

- **WHEN** 引擎對處於 `PARTIAL_FAILURE` 的配對收到「兩腿成交」內部事件
- **THEN** 狀態維持 `PARTIAL_FAILURE`，並新增一筆記錄非法轉移的事件

#### Scenario: 落地失敗即停機

- **WHEN** 配對由 `PRE_TRADE_CHECK` 轉入 `ORDER_SUBMIT` 時，store 寫入失敗（以故障注入模擬）
- **THEN** 不呼叫任何 `Executor`、引擎進入停機，且之後 `opens_exposure` 為真的 Command 被拒絕

### Requirement: Snapshot 限頻推送，行情走獨立 channel

UI SHALL 只接收限頻的 `Snapshot`；兩次推送之間的最小間隔 SHALL 為可設定值，行情更新 SHALL 不因此逐筆推送。
行情資料 SHALL 走獨立的 `watch` channel，只保留最新值，SHALL NOT 進入 `Command` 佇列，也 SHALL NOT 因 UI 讀取過慢而在 actor 內累積無界佇列。
`Snapshot` SHALL 為唯讀的複本，不含任何可用來修改引擎狀態的控制代碼。

#### Scenario: 高頻行情被合併

- **WHEN** 注入時鐘於 1 秒內送入 1,000 筆行情更新、Snapshot 最小間隔為 250 毫秒
- **THEN** UI 在該 1 秒內收到的 Snapshot 不超過 4 份，且最後一份反映最新行情

#### Scenario: UI 讀取過慢

- **WHEN** UI 端停止讀取 Snapshot 10 秒
- **THEN** actor 的記憶體用量不隨時間線性增加，且 actor 仍正常處理 tick

### Requirement: 同標的原子新增與已開啟配對的定義

新增 `PREPARED` 配對 SHALL 呼叫 store 的原子新增操作，SHALL NOT 在引擎內先查詢再寫入。
「已開啟配對」（供 `RiskLimits` 計算 `max_concurrent_pairs`）SHALL 定義為狀態不屬於 `FINALIZED`、`CANCELLED`、`BLOCKED` 的配對，因此處於 `PARTIAL_FAILURE`、`IMBALANCED`、`UNRESOLVED` 的配對 SHALL 仍計入。

#### Scenario: 警示中的配對仍佔名額

- **WHEN** `max_concurrent_pairs` 為 1，且已有一組配對處於 `PARTIAL_FAILURE`
- **THEN** 對另一標的的送單前檢查，`RiskLimits` 失敗

#### Scenario: 已終結的配對不佔名額

- **WHEN** 唯一的配對已是 `FINALIZED`
- **THEN** `RiskLimits` 以已開啟配對數 0 計算

### Requirement: 進場與成交明細寫入不可變事件

為供 `funding-pnl` 計算損益，引擎 SHALL 在 `ORDER_SUBMIT` 轉移事件（與轉移同一 transaction）中記錄進場快照：兩腿的預期價格（Node 1 計算數量所用的送單前價格）、基準價與掃描價、資金費率，以及以同一份送單前資料計算的 Net Edge（各組成與門檻）、名目與槓桿。
每筆送單結果事件（`ORDER_SUBMITTED`）與之後查詢發現的成交變化事件（`ORDER_FILL`）SHALL 記錄該委託的交易所、標的、狀態、成交數量、成交均價、手續費與手續費幣別；交易所未回報的欄位 SHALL 記為空值，SHALL NOT 以推測值填入。
`ports::OrderStatus` SHALL 帶有 `fee` 與 `fee_asset`（皆可為空）；`SimulatedExecutor` SHALL 回報手續費 0、幣別 `USDT`，其事件仍標記為模擬。

#### Scenario: 模擬進場留下完整明細

- **WHEN** SIMULATION 的配對通過 Node 0 並完整成交
- **THEN** `ORDER_SUBMIT` 事件含兩腿預期價格與 Net Edge 快照，兩筆 `ORDER_SUBMITTED` 事件各含成交價、數量、手續費 0 與幣別 `USDT`，且標記為模擬

#### Scenario: 稍後查詢才得知的成交

- **WHEN** 送單時委託尚未成交，之後以原 `client_order_id` 查詢得知已成交
- **THEN** 寫入一筆 `ORDER_FILL` 事件，含成交價、數量、手續費與手續費幣別

