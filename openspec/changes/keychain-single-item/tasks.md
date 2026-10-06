## 1. Bundle 與快取（先寫失敗測試，用計數 `KeyStore` 替身）

- [x] 1.1 `BundleSecrets<S: KeyStore>`：JSON 讀寫、空值視為未設定、redaction 維持
- [x] 1.2 程序內快取：多次 `get` 只存取一次底層；失敗也快取不重試
- [x] 1.3 舊分項遷移：bundle 不存在 → 讀舊項 → 寫 bundle、舊項保留；都不存在不建空 bundle；bundle 寫入失敗仍可用；bundle 損毀視為失敗

## 2. 接線

- [x] 2.1 `KeychainSecrets::system()`（`live.rs` 使用處）改回傳快取版 `BundleSecrets`
- [x] 2.2 `secrets_cli.rs` 的 `set` / `delete` / `import-env` / `status` 改走 bundle（`import-env` 只寫一次），更新既有 CLI 測試
- [x] 2.3 更新 `ui-trading-pages/design.md` 中「每 30 秒檢查鑰匙圈」的說明為讀快取

## 3. 驗證

- [x] 3.1 `cargo test -p tong-funding store::secrets` 與全套 `cargo test` 綠燈
- [ ] 3.2 實機：第一次啟動（遷移）記錄詢問次數；第二次啟動確認只詢問 1 次；拒絕授權時確認不再反覆跳出
