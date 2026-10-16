## ADDED Requirements

### Requirement: 每一頁固定顯示依身分的環境橫幅

程式 SHALL 在每一頁的頂端固定顯示一條環境橫幅，且橫幅 SHALL NOT 可被關閉或隱藏。橫幅內容由程式身分與執行模式決定：

- Live 身分且執行模式為對交易所下單：顯示紅色橫幅，文字包含「LIVE」、「真錢」與「Binance + Bybit 正式環境」；
- Live 身分且執行模式為模擬（影子）：顯示紅色橫幅，文字包含「LIVE」與「模擬（不下單）」；
- Demo 身分且對交易所下單：顯示琥珀色橫幅，文字包含「DEMO」與「不涉及真錢」；
- Demo 身分且為模擬：顯示灰色橫幅，文字包含「模擬」。

環境 SHALL 以文字標明，SHALL NOT 只靠顏色區分。橫幅 SHALL 同時顯示 kill switch 狀態（Live：「交易已鎖定（只能平倉）」或「交易已武裝」）。

#### Scenario: Live 對交易所下單的橫幅

- **WHEN** Live 身分、執行模式為對交易所下單、kill switch 已關閉
- **THEN** 每一頁頂端都有紅色橫幅，文字含「LIVE」「真錢」「Binance + Bybit 正式環境」與「交易已武裝」

#### Scenario: Live 啟動時的橫幅

- **WHEN** Live 身分剛啟動、kill switch 為開啟
- **THEN** 橫幅顯示「交易已鎖定（只能平倉）」

#### Scenario: Demo 與模擬的橫幅不含 LIVE

- **WHEN** Demo 身分的任一執行模式
- **THEN** 橫幅與畫面任何位置都不出現「LIVE」字樣

#### Scenario: 橫幅無法關閉

- **WHEN** 使用者在任一頁操作
- **THEN** 沒有任何控制項可以關閉或隱藏橫幅

### Requirement: 視窗標題帶環境

視窗標題 SHALL 帶環境前綴：Live 身分為 `[LIVE]`，Demo 身分為 `[DEMO]`。

#### Scenario: Live 的視窗標題

- **WHEN** Live 身分啟動
- **THEN** 視窗標題以 `[LIVE]` 開頭

### Requirement: 會花錢的確認重複顯示環境與金額

「加入交易單」「進場」「平倉」「立即平倉」與其他會送出訂單的確認對話框，SHALL 顯示目前環境（文字）與涉及的金額（名目）。Live 身分下，確認按鈕文字 SHALL 為「以真錢下單」（開倉）或「以真錢平倉」（平倉）。

#### Scenario: Live 的加入交易單確認

- **WHEN** Live 身分的使用者要加入一筆配對到交易單
- **THEN** 確認內容顯示「LIVE 真錢」與該配對每腿名目，按鈕文字為「以真錢下單」

#### Scenario: Demo 的確認不含真錢字樣

- **WHEN** Demo 身分的使用者要加入一筆配對
- **THEN** 確認內容顯示 DEMO，且不含「真錢」或「LIVE」

### Requirement: 環境文案由身分與模式窮舉產生

所有顯示環境的文字（系統通知、手動下單頁的帳本來源、執行模式說明與按鈕啟用說明、配對卡徽章）SHALL 由程式身分與執行模式以窮舉比對產生，SHALL NOT 以「是否模擬」的布林推論（不得把非模擬一律寫成 `EXCHANGE_DEMO`）。新增身分或模式變體時，這些位置 SHALL 因編譯器的窮舉檢查而報錯。

#### Scenario: Live 的系統通知寫 LIVE

- **WHEN** Live 身分發生一筆需要通知的 `PARTIAL_FAILURE`
- **THEN** 通知文字標示環境為 LIVE，而不是 DEMO

#### Scenario: Live 手動下單頁的帳本來源

- **WHEN** Live 身分開啟手動下單頁
- **THEN** 畫面標示帳戶來源為 LIVE 交易所帳戶，而不是模擬帳本

### Requirement: Live 身分下每個配對與訂單帶環境徽章

Live 身分下，配對卡、交易單列、訂單紀錄與系統日誌中與配對或訂單有關的列 SHALL 顯示 `LIVE` 徽章。

#### Scenario: 配對卡有徽章

- **WHEN** Live 身分顯示一個持倉中的配對卡
- **THEN** 卡片上有 `LIVE` 徽章
