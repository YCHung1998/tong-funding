## MODIFIED Requirements

### Requirement: 狀態列顯示模式、連線狀態與 kill switch 位置

狀態列 SHALL 顯示目前的執行模式徽章、每個交易所（Binance、Bybit、OKX）的連線狀態指示，以及 kill switch 狀態的預留位置。
在本 change 範圍內，連線狀態 SHALL 一律顯示為「未連線」，kill switch 位置 SHALL 顯示為「未啟用」。
環境標示 SHALL 依程式身分決定（見 `live-banners`）：Live 身分 SHALL 清楚顯示「LIVE」；Demo 與模擬的畫面 SHALL NOT 出現「LIVE」字樣。

#### Scenario: 首次啟動的預設模式為 SIMULATION

- **WHEN** 使用者首次啟動程式（尚無任何已儲存設定）
- **THEN** 狀態列的模式徽章顯示 `SIMULATION`，而不是任何可下單的模式

#### Scenario: Demo 身分顯示 Demo/Testnet 標示

- **WHEN** Demo 身分的使用者查看標題列或狀態列
- **THEN** 畫面上有明確的「Demo / Testnet」標示，且不出現「LIVE」字樣

#### Scenario: Live 身分顯示 LIVE

- **WHEN** Live 身分的使用者查看標題列或狀態列
- **THEN** 畫面上有明確的「LIVE」標示，且沒有「Demo / Testnet」標示
