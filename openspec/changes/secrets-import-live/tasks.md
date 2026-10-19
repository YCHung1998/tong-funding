## 1. 匯入

- [ ] 1.1 紅色測試先行（`app/src/store/secrets_cli.rs` tests）：demo 讀 `DEMO_*` 與無前綴、忽略 `LIVE_*`；live 讀 `LIVE_*` 與無前綴、忽略 `DEMO_*`；衝突零寫入；`LIVE_OKX_*` 拒絕；與 demo 同值拒絕；live 只寫 `tong-funding-live`；輸出不含值；無可存值非零結束
- [ ] 1.2 實作 `import-env [--live]`，移除互斥；更新用法與文件註解；單次 bundle 寫入
- [ ] 1.3 檔案權限警告（群組或其他人可讀）

## 2. 文件與範例

- [ ] 2.1 `.env.example`（佔位值、中英註解、兩種用法與指令）；`.gitignore` 忽略 `.env*` 例外 `.env.example`
- [ ] 2.2 更新 `docs/live-runbook.md`、`docs/real-trading-plan.md`；同步 `live-process` 的 spec／design／proposal／tasks 中「import-env 拒絕 live」的描述

## 3. 驗證

- [ ] 3.1 `openspec validate secrets-import-live` 與 `live-process`；`cargo test -p tong-funding-core -p tong-funding` 乾淨目錄全綠並記錄通過數
