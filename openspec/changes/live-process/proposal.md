## Why

使用者要把 Binance 與 Bybit 打通到**正式環境（真錢）**交易。目前整個系統刻意只能碰 demo／testnet：簽名請求的主機是編譯期寫死的 demo 主機，傳輸層有白名單，靜態檢查禁止在簽名程式碼出現正式主機，金鑰與資料庫也沒有環境區分。

2026-10-15 的三鏡頭審查（skeptic、red-team、simplifier，見 `docs/real-trading-plan.md`）一致認為：在同一個程式裡做「模擬／demo／live」三態切換很危險（重啟或解除武裝會讓 live 部位平不掉、相等比較在新增模式時靜默放行、環境容易用錯）。使用者同意改為**啟動時就決定、整個程式不會改變的身分**：`tong-funding --live`。本變更建立這個身分與它的所有安全底層；警示畫面（橫幅、標題、確認對話框）在 `live-banners`，武裝與硬上限在之後的 `live-guardrails`。

## What Changes

- **程式身分**：新增 `ProcessIdentity { Demo, Live }`，只由啟動參數 `--live` 決定（不讀環境變數、不讀設定檔），整個程序期間不變。不帶 `--live` 等同現在的行為（模擬＋demo）；`--live` 搭配其他子命令（bench、secrets 等）的組合依規則處理（見 spec）。
- **單一實例鎖**：同一時間只能跑一個程式（不分 demo 或 live、不分資料庫），避免兩個程式共用 IP 權重與鑰匙圈。
- **傳輸層主機政策**：新增 `HostPolicy::SignedLive` 與獨立的 live 主機清單（Binance `fapi.binance.com`、Bybit `api.bybit.com`），**不併入**現有的 `ALLOWED_SIGNED_HOSTS`；demo 傳輸拒絕 live 主機，live 傳輸拒絕 demo 主機；公開傳輸拒絕所有交易所的認證標頭與 `signature`／`sign` 參數。
- **主機常數單一來源**：live 主機字串只出現在 `signed/endpoints.rs`；靜態檢查擴大到整個 `src` 掃描正式主機字串，只放行該檔案與既有的公開客戶端。
- **限頻分類改依呼叫端型別**，不再用「主機是否在簽名白名單」判斷（live 簽名與公開行情同一台主機，主機判斷會失準）。
- **資料庫與身分綁定**：live 身分使用 `funding-live.db`；資料庫寫入不可變的環境中繼資料，開檔時與程式身分比對，不符就停機；schema 版本提升，讓舊版程式拒絕開啟。
- **鑰匙圈**：live 身分只讀獨立的鑰匙圈項目（service `tong-funding-live`）；demo 身分永遠不讀它；`secrets` 子命令以 `--live` 指定目標，`import-env` 的 live 行為見 `secrets-import-live`。
- **客戶端依身分選主機**：帳戶、持倉、槓桿上限、資金費流水、費率來源、時鐘校時、下單，在 live 身分下全部走 live 主機，且**不會建立任何 demo 客戶端**（反之亦然）。
- **訂單 ID 前綴**新增 `live`，讓重啟對帳辨識 live 單。
- **live 身分排除 OKX**：引擎拒絕含 OKX 腿的配對，執行器只建 Binance 與 Bybit，掃幣可交易清單只含這兩所。
- **BREAKING（對 live 而言是新行為，對現有 demo 使用者不變）**：現有 `funding.db` 在 demo 身分首次開啟時會被標上 `demo` 環境，並提升 schema 版本；舊版二進位之後無法開啟新的資料庫。

## Capabilities

### New Capabilities

- `live-process`：程式身分、單一實例鎖、依身分的傳輸與主機政策、資料庫環境綁定、live 客戶端選擇、訂單 ID 前綴、live 排除 OKX。

### Modified Capabilities

- `secret-bundle`：新增 live 專用的鑰匙圈項目與 CLI 的 `--live` 行為，`import-env` 的 live 行為見 `secrets-import-live`。

## Impact

- 程式：`app/src/main.rs`（身分與鎖）、`app/src/exchange/signed/endpoints.rs`、`app/src/exchange/reqwest_transport.rs`、`app/src/exchange/execution/http.rs`、`app/src/exchange/static_checks.rs`、`app/src/exchange/health/ratelimit.rs`、`app/src/store/db.rs`（環境中繼資料與 schema v5）、`app/src/store/secrets.rs`／`secrets_cli.rs`、`app/src/ui/live.rs`（客戶端選擇）、`app/src/engine/ids.rs`／`recovery.rs`、`app/src/ui/vm/bridge.rs`（可交易清單）。
- 資料：新增 `funding-live.db`（新檔案）；既有 `funding.db` 增加環境中繼資料並提升 schema 版本。
- 外部：live 程式會連線到正式主機（`fapi.binance.com`、`api.bybit.com`）；這是本變更刻意開啟的能力，受上述政策與測試約束。
- 不涵蓋：武裝流程與硬上限（`live-guardrails`）、橫幅與標題（`live-banners`）、IOC 限價單、金鑰權限 API 預檢、每日對帳與損失上限（延後，開 AUTO 之前）。
- 相依：先於 `live-banners` 與 `live-guardrails`。
