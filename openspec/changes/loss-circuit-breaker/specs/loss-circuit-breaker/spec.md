## ADDED Requirements

### Requirement: 熔斷的計算區間

熔斷 SHALL 只計入「最近一次開放交易」之後的配對。區間起點 SHALL 為最近一筆 `KILL_SWITCH_CHANGED`（`on` 為 false）的時間；不存在時 SHALL 為本功能第一次啟動時寫入的 `LOSS_BREAKER_BASELINE` 事件時間。之前的歷史 SHALL NOT 計入。

#### Scenario: 升級後不因舊虧損停機

- **WHEN** 資料庫有 17 筆舊虧損配對、從未關閉過 kill switch，且第一次以本版本啟動
- **THEN** 寫入 `LOSS_BREAKER_BASELINE`，累計虧損為 0，不觸發

#### Scenario: 重新開放交易即重置

- **WHEN** 熔斷觸發後使用者關閉 kill switch
- **THEN** 新區間從那一刻開始，累計虧損與連虧歸零

### Requirement: 計入的配對與金額

區間內寫入 `CLOSE_CONFIRMED` 的配對 SHALL 依該事件時間排序計入。每筆金額 SHALL 為：最新 PnL 淨額已知時用淨額；否則用由目前成交記錄重算的「價格損益 − 開倉手續費 − 平倉手續費」（不含資金費）；兩者皆無法取得時為未知。

#### Scenario: 資金費尚未入帳

- **WHEN** 一筆配對已平倉，價格損益 −2.0、手續費 1.9，資金費流水尚未取得
- **THEN** 該筆以 −3.9 計入

#### Scenario: 淨額可用後改用淨額

- **WHEN** 同一筆之後的 PnL 淨額為 +0.5
- **THEN** 該筆改以 +0.5 計入，連虧計數依新值重算

### Requirement: 觸發條件與效果

當區間內累計金額 ≤ −`loss_breaker_max_loss_usdt`，或結尾連續虧損（金額 < 0）筆數 ≥ `loss_breaker_max_consecutive_losses`，或任一計入配對的金額未知時，系統 SHALL 打開 kill switch，並在同一交易寫入 `KILL_SWITCH_CHANGED` 與 `LOSS_BREAKER_TRIPPED`（含累計值、連虧數、門檻、計入的配對與原因）、發出通知；Live SHALL 同時解除武裝。
熔斷 SHALL 只擋開新倉；進行中的配對 SHALL 照常出場，平倉與人工處理 SHALL 照常可用。同一區間 SHALL 只觸發一次。

#### Scenario: 連虧達上限

- **WHEN** 上限為 3 筆，區間內依序 −0.5、−0.3、−1.2
- **THEN** 第三筆計入時觸發，kill switch 為開啟，`LOSS_BREAKER_TRIPPED` 的原因為連虧

#### Scenario: 獲利重置連虧

- **WHEN** 依序 −0.5、+0.2、−0.3、−1.2，上限 3 筆
- **THEN** 不因連虧觸發（結尾連虧 2 筆）

#### Scenario: 金額未知

- **WHEN** 一筆已平倉配對的成交價格無法取得
- **THEN** 觸發，原因寫明「損益未知」

### Requirement: 熔斷狀態公開

引擎 SHALL 持續發布熔斷狀態：區間起點、累計金額、連虧筆數、兩個門檻、是否已觸發與原因，供狀態列與風控頁顯示。

#### Scenario: 狀態列顯示額度

- **WHEN** 區間內累計 −6.2 USDT、連虧 1 筆，門檻 15 與 3
- **THEN** 狀態列顯示「熔斷：已虧 6.20／15 USDT · 連虧 1／3」
