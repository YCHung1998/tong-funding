## ADDED Requirements

### Requirement: API 金鑰只存放於 macOS Keychain

每個交易所的 API key、secret 與（OKX 的）passphrase SHALL 存放於 macOS Keychain，SHALL NOT 寫入資料庫、設定檔、環境檔或日誌。
程式 SHALL 透過單一存取介面讀取金鑰，使測試能以記憶體版本替代，而不碰真實 Keychain。
讀取金鑰失敗 SHALL 使該交易所被視為「未連線」，不得以空字串或預設值嘗試簽名請求。

#### Scenario: 金鑰不出現在資料庫

- **WHEN** 使用者儲存某交易所的金鑰後，掃描整個資料庫檔案內容
- **THEN** 找不到該 key 或 secret 的字串

#### Scenario: 讀取失敗不退而求其次

- **WHEN** Keychain 回報讀取失敗或項目不存在
- **THEN** 該交易所的簽名功能不可用，狀態為「未連線」，且不送出任何請求

### Requirement: 資料庫檔案權限為 0600

資料庫檔案（含 WAL 與 SHM 附屬檔）SHALL 僅限擁有者讀寫（權限 0600）。
建立資料庫時 SHALL 先設定權限再寫入資料；啟動時若發現權限比 0600 寬鬆，SHALL 修正並記錄事件。

#### Scenario: 新建資料庫的權限

- **WHEN** 程式首次建立資料庫
- **THEN** 主檔與附屬檔的權限皆為 0600

#### Scenario: 權限過寬時修正

- **WHEN** 啟動時發現資料庫權限為 0644
- **THEN** 權限被修正為 0600，並寫入一筆事件記錄此修正

### Requirement: 日誌與事件寫入前遮蔽簽名與金鑰

任何寫入日誌或 `events.payload` 的字串 SHALL 先經過遮蔽函式。下列內容的值 SHALL 被替換為固定占位文字：
URL 查詢參數 `signature`、`api_key`、`apiKey`；
標頭 `X-MBX-APIKEY`、`X-BAPI-API-KEY`、`X-BAPI-SIGN`、`OK-ACCESS-KEY`、`OK-ACCESS-SIGN`、`OK-ACCESS-PASSPHRASE`。
其他參數（例如 `symbol`、`timestamp`）SHALL 原樣保留，以利除錯。
遮蔽 SHALL 套用於例外訊息（例如連線錯誤字串內含完整 URL 的情況）。

#### Scenario: 例外訊息含簽名

- **WHEN** 連線逾時的錯誤字串內含 `...?symbol=BTCUSDT&timestamp=1&signature=abcdef`
- **THEN** 寫入事件前，`signature` 的值被替換為占位文字，其餘參數保留

#### Scenario: 標頭值被遮蔽

- **WHEN** 要記錄的請求描述含 `X-MBX-APIKEY: realkey`
- **THEN** 記錄中的值為占位文字，不含 `realkey`
