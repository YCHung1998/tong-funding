## ADDED Requirements

### Requirement: 憑證以單一鑰匙圈項目儲存

所有交易所憑證 SHALL 儲存在同一個鑰匙圈項目（service `tong-funding`、account `credentials`）中，內容為以 `<Exchange>:<name>` 為鍵的 JSON 物件。
空字串值 SHALL 視為未設定。
錯誤訊息與日誌 SHALL NOT 包含任何憑證值（沿用既有的 redaction）。

#### Scenario: 寫入一個值不影響其他值

- **WHEN** bundle 已有 Binance 的 key 與 secret，使用者以 `secrets set Bybit api_key` 寫入 Bybit key
- **THEN** bundle 同時含有三個值，Binance 的值不變

### Requirement: 每個程序只讀一次鑰匙圈

程式 SHALL 在第一次需要憑證時讀取 bundle 一次並快取於記憶體；同一程序之後的所有憑證讀取 SHALL 只讀快取，SHALL NOT 再存取鑰匙圈。
讀取失敗（含使用者拒絕授權）SHALL 被快取為失敗，同一程序內 SHALL NOT 重試，所有憑證讀取回傳「未連線」錯誤；重新啟動程式才重試。

#### Scenario: 多次讀取只觸發一次鑰匙圈存取

- **WHEN** 啟動後依序讀取 Binance key、Binance secret、Bybit key、Bybit secret，並在之後每 30 秒再讀一次
- **THEN** 底層鑰匙圈存取恰好發生一次

#### Scenario: 拒絕授權後不反覆詢問

- **WHEN** 使用者在授權視窗按「拒絕」
- **THEN** 之後的每次讀取都回傳失敗，且鑰匙圈存取次數維持一次

### Requirement: 從舊的分項格式自動遷移

讀取時若 bundle 項目不存在，系統 SHALL 讀取舊的分項（`<Exchange>:<name>` 各自為一個 account），把存在且非空的值寫成 bundle，再以該 bundle 作為快取。
舊項目 SHALL 保留不刪除。
bundle 寫入失敗時 SHALL 仍使用讀到的值運作，並記錄一筆警告；下次啟動再嘗試遷移。
舊分項與 bundle 都不存在時 SHALL 視為「未設定」，SHALL NOT 建立空的 bundle。

#### Scenario: 首次遷移

- **WHEN** 只有舊的 4 個分項存在
- **THEN** 讀取後 bundle 含這 4 個值，舊分項仍存在，下一個程序只存取 bundle 一次

#### Scenario: bundle 已存在

- **WHEN** bundle 存在
- **THEN** 系統不讀取任何舊分項

### Requirement: CLI 子指令操作 bundle

`secrets set`、`delete`、`import-env` SHALL 讀取 bundle 後修改再整筆寫回；`import-env` SHALL 只寫一次 bundle。
`secrets status` SHALL 只讀取 bundle 一次並列出每個憑證是否已設定（不顯示值）。

#### Scenario: import-env 只寫一次

- **WHEN** 以含 7 個值的 env 檔執行 `secrets import-env`
- **THEN** 底層鑰匙圈寫入恰好一次，bundle 含 7 個值
