## Context

`secrets_cli::import_env` 讀 Python 版 `.env` 的七個無前綴名稱，寫入選定身分的 bundle。`live-process` 4.1 讓 `import-env` 與 `--live` 互斥。使用者決定改為兩者都支援，同檔或分檔皆可。

## Decisions

1. **命名**：`DEMO_<EX>_<NAME>`、`LIVE_<EX>_<NAME>`；無前綴舊名稱（`BINANCE_API_KEY`…、`OKX_DEMO_*`）對選定身分有效。例：`.env-real` 內容為無前綴名稱，執行 `import-env --live .env-real` 即寫入 live。
2. **隔離**：選定 demo 時 `LIVE_*` 名稱不讀；選定 live 時 `DEMO_*` 不讀；兩者都只回報 `ignored NAME`。這使同一檔案可放兩組金鑰而不會寫錯格。
3. **衝突**：同一個秘密有兩個來源名稱且值不同、或同名重複且值不同，整批拒絕且零寫入，訊息只列變數名。
4. **Live 防護**：OKX 在 live 不存在（`LIVE_OKX_*` 拒絕，無前綴 `OKX_DEMO_*` 在 live 視為 `ignored`）；寫入 live 的值若等於 demo 項目同格現值則整批拒絕（透過 demo 的 `BundleSecrets` 唯讀比對，不寫 demo、不印值）；檔案權限含群組或其他人讀取位元時警告但不拒絕；live 匯入沒有任何可存的值時非零結束。
5. **身分隔離**：live 匯入只寫 `tong-funding-live`；為了比對只會讀（不寫）demo 項目。demo 匯入不碰 live 項目。
6. **輸出**：只列 `stored <exchange> <secret>`、`ignored NAME`、`skipped … (empty)`，不含值。
7. **解析**沿用既有規則（註解、`export `、引號、CRLF）；一次 bundle 寫入。

## Risks / Trade-offs

- [使用者把 `.env-real` 以不帶 `--live` 匯入 demo] → 無法由程式分辨無前綴檔案的用途；`.env.example` 與 runbook 明確寫出；建議 live 金鑰用 `LIVE_*` 前綴。demo 匯入後 live 匯入的「與 demo 相同」防護能攔下貼錯。
- [live 比對需讀 demo 項目] → 多一次 Keychain 讀取提示，換取攔截貼錯金鑰。
