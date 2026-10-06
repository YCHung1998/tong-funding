## Why

每次啟動程式，macOS 會跳出 4 次鑰匙圈授權詢問。原因是 API 憑證分成 4 個獨立的鑰匙圈項目（Binance / Bybit 各一組 key 與 secret），macOS 對每個項目各自詢問；程式也沒有在記憶體快取，每次簽名請求都重讀鑰匙圈。使用者應該只需要授權一次。

## What Changes

- 所有交易所憑證改存為**單一**鑰匙圈項目（service `tong-funding`、account `credentials`，內容為 JSON），啟動後第一次需要時讀一次並快取在記憶體，整個程序期間不再讀鑰匙圈 → 每次啟動最多 1 次授權詢問（選「永遠允許」則到下次重新編譯前都不再詢問）。
- **自動遷移**：合併項目不存在時，讀取舊的分項（僅這一次會照舊詢問各項），寫入合併項目；舊項目保留不刪，可回退。
- 讀取被拒或失敗時，記憶體記住失敗結果，不在每 30 秒的輪詢中反覆跳詢問；狀態顯示「未連線（鑰匙圈讀取失敗）」，重新啟動程式即重試。
- `secrets` 子指令（`set`、`delete`、`import-env`、`status`）改為讀改寫合併項目；`status` 只觸發一次詢問。

## Capabilities

### New Capabilities
- `secret-bundle`: 憑證以單一鑰匙圈項目儲存、程序內快取、舊格式遷移與失敗處理。

### Modified Capabilities
（無已封存的 secrets spec；`store-sqlite` change 的 `secret-storage` 需求「單一存取介面 + 記憶體替身、讀取失敗視為未連線」維持不變。）

## Impact

- `app/src/store/secrets.rs`：新增 bundle 格式與快取 provider；`KeyStore` 介面不變。
- `app/src/store/secrets_cli.rs`：各子指令改走 bundle。
- `app/src/ui/live.rs`：沿用同一個 `Arc<dyn SecretProvider>`（已共用），改成快取版。
- 相關文件：`openspec/changes/ui-trading-pages/design.md` 第 141 行「每 30 秒檢查鑰匙圈」補註為讀快取。
