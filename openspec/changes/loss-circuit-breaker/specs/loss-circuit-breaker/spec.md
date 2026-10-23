## ADDED Requirements

### Requirement: 熔斷的計算區間

熔斷 SHALL 只計入「最近一次開放交易」之後的配對。區間起點 SHALL 為最近一筆「前一個狀態為 ON」的 `KILL_SWITCH_CHANGED`（`on` 為 false）的時間（沒有實際切換的 OFF，例如 kill switch 本來就是關的，SHALL NOT 重置區間）；不存在時 SHALL 為本功能第一次啟動時寫入的 `LOSS_BREAKER_BASELINE` 事件時間。之前的歷史 SHALL NOT 計入。

#### Scenario: 升級後不因舊虧損停機

- **WHEN** 資料庫有 17 筆舊虧損配對、從未關閉過 kill switch，且第一次以本版本啟動
- **THEN** 寫入 `LOSS_BREAKER_BASELINE`，累計虧損為 0，不觸發

#### Scenario: 重新開放交易即重置

- **WHEN** 熔斷觸發後使用者關閉 kill switch
- **THEN** 新區間從那一刻開始，累計虧損與連虧歸零

#### Scenario: 沒有實際切換的 OFF 不重置

- **WHEN** 區間內已有一筆虧損，之後又寫入一筆 `KILL_SWITCH_CHANGED`（`on` 為 false）而 kill switch 事前就是關的
- **THEN** 區間起點不動，那筆虧損仍然計入

### Requirement: 計入的配對與金額

配對在區間內第一次出現下列任一事件時 SHALL 計入，並依其中較早者的時間排序：`CLOSE_CONFIRMED`，或「從 `PARTIAL_FAILURE`／`IMBALANCED`／`UNRESOLVED` 轉入 `FINALIZED`」的 `PAIR_TRANSITION`（使用者人工確認已平倉；這類配對不會有 `CLOSE_CONFIRMED`，虧損卻是真的）。每個配對只計一次；第一次出現在區間起點之前的配對屬於較早的區間，SHALL NOT 重複計入；模擬配對 SHALL NOT 計入。每筆金額 SHALL 為：最新 PnL 淨額已知時用淨額；否則用由目前成交記錄重算的「價格損益 − 開倉手續費 − 平倉手續費」（不含資金費）；兩者皆無法取得時為未知。

#### Scenario: 資金費尚未入帳

- **WHEN** 一筆配對已平倉，價格損益 −2.0、手續費 1.9，資金費流水尚未取得
- **THEN** 該筆以 −3.9 計入

#### Scenario: 單腿、看門狗平倉、人工確認後計入

- **WHEN** 配對單腿成交進入 `PARTIAL_FAILURE`，看門狗平掉該腿，使用者按「人工確認已平倉」轉入 `FINALIZED`
- **THEN** 該配對從轉入 `FINALIZED` 的那一刻起計入，金額規則同上，達門檻時觸發

#### Scenario: 淨額可用後改用淨額

- **WHEN** 同一筆之後的 PnL 淨額為 +0.5
- **THEN** 該筆改以 +0.5 計入，連虧計數依新值重算

### Requirement: 觸發條件與效果

當區間內累計金額 ≤ −`loss_breaker_max_loss_usdt`，或結尾連續虧損（金額 < 0）筆數 ≥ `loss_breaker_max_consecutive_losses`，或任一計入配對的金額未知時，系統 SHALL 打開 kill switch，並在同一交易寫入 `KILL_SWITCH_CHANGED` 與 `LOSS_BREAKER_TRIPPED`（含累計值、連虧數、門檻、計入的配對與原因）、發出通知；Live SHALL 同時解除武裝。
kill switch 已經是開啟（手動或看門狗停止）時達到門檻，若本區間尚無觸發紀錄，系統 SHALL 仍以同一交易寫入 `KILL_SWITCH_CHANGED`（`cause` 為 `LOSS_BREAKER_TRIPPED`）與 `LOSS_BREAKER_TRIPPED` 並發出通知，狀態列的停止原因 SHALL 顯示「熔斷」。
熔斷 SHALL 只擋開新倉；進行中的配對 SHALL 照常按排程出場（提前出場會暫停，因為 kill switch 開啟時提前出場不動作），平倉與人工處理 SHALL 照常可用。同一區間 SHALL 只觸發一次。
熔斷評估 SHALL 失敗即封閉：讀取事件（含監看的 watermark）失敗而無法評估時，系統 SHALL 打開 kill switch、寫入 `LOSS_BREAKER_TRIPPED`（原因 `UNREADABLE`「無法評估熔斷」），Live 同時解除武裝並記錄鎖定原因，並發出通知。
風控設定被修改（`RISK_CONFIG_UPDATED`）後 SHALL 立即重新評估，狀態列數字隨新門檻更新。在 App 內顯示警示並記錄於系統日誌是目前的通知方式（尚未接上 macOS 系統通知）。

#### Scenario: 連虧達上限

- **WHEN** 上限為 3 筆，區間內依序 −0.5、−0.3、−1.2
- **THEN** 第三筆計入時觸發，kill switch 為開啟，`LOSS_BREAKER_TRIPPED` 的原因為連虧

#### Scenario: 獲利重置連虧

- **WHEN** 依序 −0.5、+0.2、−0.3、−1.2，上限 3 筆
- **THEN** 不因連虧觸發（結尾連虧 2 筆）

#### Scenario: 停止中達到門檻

- **WHEN** 使用者已手動打開 kill switch，區間內進行中的配對陸續平倉，連虧達上限
- **THEN** 寫入 `LOSS_BREAKER_TRIPPED` 並通知，狀態列顯示「交易：已停止（熔斷）」

#### Scenario: 改門檻後立即生效

- **WHEN** 區間內已虧 2 USDT，使用者把上限從 15 改為 2
- **THEN** 不必等下一筆平倉，下一輪評估就觸發，狀態列的上限數字同時更新

#### Scenario: 讀不到事件

- **WHEN** 熔斷評估讀取事件失敗
- **THEN** kill switch 打開、寫入原因 `UNREADABLE` 並通知，Live 的武裝面板顯示鎖定原因

#### Scenario: 金額未知

- **WHEN** 一筆已平倉配對的成交價格無法取得
- **THEN** 觸發，原因寫明「損益未知」

### Requirement: 熔斷狀態公開

引擎 SHALL 持續發布熔斷狀態：區間起點、累計金額、連虧筆數、兩個門檻、是否已觸發與原因，供狀態列與風控頁顯示。每一筆 `KILL_SWITCH_CHANGED` SHALL 以 `cause`（原因事件的類型，普通切換沒有）標明來源，停止原因 SHALL 讀這個欄位，不得靠相鄰事件的 id 推斷。

#### Scenario: 狀態列顯示額度

- **WHEN** 區間內累計 −6.2 USDT、連虧 1 筆，門檻 15 與 3
- **THEN** 狀態列顯示「熔斷：已虧 6.20／15 USDT · 連虧 1／3」
