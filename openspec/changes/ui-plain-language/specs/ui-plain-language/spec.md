## ADDED Requirements

### Requirement: 停用的交易所不製造雜訊、不隱藏曝險

不在 `allowed_exchanges` 的交易所，在總覽、交易單、持倉與合約設定頁 SHALL 只顯示一行弱化文字「<交易所> 已停用（風控設定未勾選）」，錯誤細節 SHALL 只在展開時顯示。該交易所若有持倉，持倉 SHALL 照常顯示並加註「此交易所已停用，仍有持倉」。總資產的說明 SHALL 只列出計入的交易所。

#### Scenario: OKX 已停用且帳戶讀取失敗

- **WHEN** OKX 未勾選，OKX 帳戶讀取回傳 `acct_mode_unsupported`
- **THEN** 總覽、交易單、持倉、合約設定都只顯示「OKX 已停用（風控設定未勾選）」，沒有紅色或黃色錯誤字

#### Scenario: 停用的交易所仍有持倉

- **WHEN** OKX 未勾選，但 OKX 有一筆持倉
- **THEN** 持倉頁照常列出該持倉並標示「此交易所已停用，仍有持倉」

### Requirement: 內部代碼以白話呈現

配對狀態、執行環境、觸發模式、PnL 狀態，以及已收錄的交易所錯誤碼，畫面 SHALL 以中文名稱為主要文字，內部代碼只作為次要文字或說明。對照 SHALL 集中在單一模組並涵蓋所有配對狀態；未收錄的代碼 SHALL 原樣顯示。

#### Scenario: 配對狀態

- **WHEN** 顯示狀態為 `FINALIZED`、`BLOCKED`、`PARTIAL_FAILURE` 的配對
- **THEN** 主要文字分別為「已完成」「被擋下」「單腿失敗」

#### Scenario: 已收錄的錯誤碼

- **WHEN** 錯誤訊息含 `acct_mode_unsupported`
- **THEN** 顯示「OKX 帳戶模式不支援」與建議動作（在 OKX 改為單幣種或跨幣種保證金模式）

### Requirement: 上次執行結果可一眼看懂

「上次執行結果」的每一筆 SHALL 顯示日期與時間（UTC）、幣種、中文結果；被擋下時 SHALL 列出原因；已完成且已有 PnL 時 SHALL 顯示淨損益；幣種與交易所 SHALL NOT 為空白。訂單編號 SHALL 保留為次要文字。

#### Scenario: 送單前檢查被擋下

- **WHEN** PTBUSDT 在 12:00:00 UTC 因送單前重抓逾時被擋下
- **THEN** 該筆顯示「10-22 12:00:00 UTC · PTBUSDT · 被擋下：送單前重抓價格逾時」

#### Scenario: 跨日的紀錄

- **WHEN** 有 10-21 15:59:50 與 10-22 00:59:50 兩筆
- **THEN** 兩筆都帶日期，依時間由新到舊排列
