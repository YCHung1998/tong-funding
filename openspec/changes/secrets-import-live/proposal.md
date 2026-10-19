## Why

使用者手上已有 `.env-demo`、`.env-real`，且輸入金鑰一條一條很難不出錯。先前 `live-process` 規定 `secrets import-env` 拒絕 `--live`（避免 `.env` 變成 live 憑證），但使用者明確要求 demo 與 live 都能從檔案匯入，並要求提供可照填的 `.env.example`。這是使用者推翻先前規則的決定；本變更在保留身分隔離的前提下提供安全的匯入。

## What Changes

- `secrets import-env [--live] <path>`：未帶 `--live` 寫 demo 項目（`tong-funding`），帶 `--live` 寫 live 項目（`tong-funding-live`）。
- 變數名：`DEMO_*`、`LIVE_*` 前綴，以及既有的無前綴名稱（對選定的身分有效）。另一個身分的前綴名稱永遠不讀（只回報 `ignored NAME`）。
- 同一個秘密以前綴與無前綴兩種名稱出現且值不同、或同名重複且值不同：整批拒絕，零寫入。
- Live 專屬防護：OKX 不寫入 live（`LIVE_OKX_*` 被拒絕）；要寫入 live 的值若與 demo 項目現存同一格的值相同（疑似貼錯 demo 金鑰）整批拒絕；檔案權限對群組或其他人可讀時印出警告；沒有任何可存的值時以非零結束。
- 新增 `.env.example`（只含佔位值）並讓 `.gitignore` 忽略 `.env*`（`.env.example` 除外）。
- 更新 `docs/live-runbook.md` 與用法文字。

## Impact

- Capabilities：`secret-bundle`（`live-process` 尚未封存的 delta 同步改為不再禁止 live 匯入）。
- 程式：`app/src/store/secrets_cli.rs`；文件：`docs/live-runbook.md`、`docs/real-trading-plan.md`、`.env.example`、`.gitignore`。
- 不變：執行中的 live 程式仍不讀環境變數或 dotenv；`import-env` 仍只是人工執行的 CLI。
