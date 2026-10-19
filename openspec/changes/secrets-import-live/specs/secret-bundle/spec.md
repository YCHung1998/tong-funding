## ADDED Requirements

### Requirement: import-env 以 --live 指定目標並隔離兩個身分的變數

`secrets import-env [--live] <path>` SHALL 把檔案中屬於選定身分的憑證寫入該身分的鑰匙圈項目：未帶 `--live` 寫 `tong-funding`，帶 `--live` 寫 `tong-funding-live`。選定 demo 時 SHALL 讀取 `DEMO_*` 與無前綴名稱，SHALL NOT 讀取 `LIVE_*`；選定 live 時 SHALL 讀取 `LIVE_*` 與無前綴名稱，SHALL NOT 讀取 `DEMO_*`。未讀取的名稱 SHALL 只以名稱回報為 `ignored`。輸出 SHALL NOT 包含任何憑證值。

#### Scenario: 同一個檔案分別匯入兩個身分

- **WHEN** 檔案同時含 `DEMO_BINANCE_API_KEY=d` 與 `LIVE_BINANCE_API_KEY=l`，先執行 `import-env`，再執行 `import-env --live`
- **THEN** demo 項目的 Binance key 為 `d`、live 項目的為 `l`，各自的執行都把另一組回報為 `ignored`

#### Scenario: 無前綴名稱跟隨選定身分

- **WHEN** `.env-real` 只含 `BINANCE_API_KEY` 與 `BINANCE_API_SECRET`，執行 `import-env --live .env-real`
- **THEN** 這兩個值寫入 live 項目，demo 項目沒有任何寫入

### Requirement: 衝突、OKX 與貼錯金鑰整批拒絕

同一個秘密以前綴與無前綴名稱出現且值不同，或同名重複且值不同時，匯入 SHALL 整批拒絕且零寫入，訊息只列變數名。Live 匯入 SHALL NOT 寫入 OKX 憑證，`LIVE_OKX_*` 名稱 SHALL 導致整批拒絕。Live 匯入若某個值等於 demo 項目同一格的現值，SHALL 整批拒絕且零寫入，且 SHALL NOT 寫入 demo 項目。Live 匯入沒有任何可寫入的值時 SHALL 以非零結束。檔案權限對群組或其他人可讀時 SHALL 印出警告但不拒絕。

#### Scenario: live 值與 demo 相同

- **WHEN** demo 項目的 Bybit key 為 `x`，`.env` 的 `LIVE_BYBIT_API_KEY=x`，執行 `import-env --live`
- **THEN** 命令失敗，live 項目與 demo 項目都沒有寫入，輸出不含值

#### Scenario: 前綴與無前綴衝突

- **WHEN** 檔案含 `BINANCE_API_KEY=a` 與 `DEMO_BINANCE_API_KEY=b`，執行 `import-env`
- **THEN** 命令失敗且零寫入，訊息列出兩個變數名
