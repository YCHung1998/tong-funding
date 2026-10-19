## ADDED Requirements

### Requirement: Live 憑證使用獨立的鑰匙圈項目

Live 身分的憑證 SHALL 儲存在獨立的鑰匙圈項目（service `tong-funding-live`、account `credentials`），格式與既有 bundle 相同。Live 身分 SHALL 只讀取該項目、SHALL NOT 讀取既有的 `tong-funding` 項目，也不做舊分項遷移；Demo 身分 SHALL NOT 讀取 live 項目。Live 項目不存在時 SHALL 視為「未設定」，SHALL NOT 退回使用 demo 憑證。

#### Scenario: Live 不會退回使用 demo 憑證

- **WHEN** Live 身分啟動，live 項目不存在但 demo 項目存在
- **THEN** 所有憑證讀取回傳「未設定」，沒有使用 demo 憑證

#### Scenario: Demo 不讀 live 項目

- **WHEN** Demo 身分啟動
- **THEN** 對 `tong-funding-live` 的鑰匙圈存取次數為 0

### Requirement: secrets 子命令以 --live 指定目標

`secrets set`、`delete`、`status` SHALL 接受 `--live` 以操作 live 項目，未帶 `--live` 時只操作既有項目。`secrets import-env` 的 `--live` 行為由變更 `secrets-import-live` 定義。

#### Scenario: set 以 --live 寫入 live 項目

- **WHEN** 執行 `secrets set --live Bybit api_key`
- **THEN** 只有 `tong-funding-live` 項目被修改，既有項目不變
