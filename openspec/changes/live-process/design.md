## Context

- 簽名請求目前只有 demo／testnet：主機是編譯期常數（`app/src/exchange/signed/endpoints.rs`：`BinanceHost { Testnet, Demo }`、`BybitHost { Demo }`、`ALLOWED_SIGNED_HOSTS`）；傳輸層還有第二道防線 `HostPolicy`（`app/src/exchange/reqwest_transport.rs:43-58`），`SignedDemo` 用 `ALLOWED_SIGNED_HOSTS`；`PublicProduction` 只拒絕 OKX 的標頭，**不拒絕** Binance／Bybit 的簽名標頭或 `signature=`。
- 靜態檢查 `app/src/exchange/static_checks.rs` 只掃 `src/exchange` 與 `ui/live.rs`，正式主機（`PRODUCTION_HOSTS`）只允許出現在公開客戶端。
- 限頻器 `classify_request`（`app/src/exchange/health/ratelimit.rs:96-105`）用「主機是否在 `ALLOWED_SIGNED_HOSTS`」判斷簽名請求。
- 鑰匙圈是單一項目（service `tong-funding`、account `credentials`，`app/src/store/secrets.rs`）；`KeychainStore::with_service` 已能指定其他 service；`secrets import-env` 會把 `BINANCE_API_KEY` 寫入無前綴欄位。
- 資料庫路徑由 `default_db_path_from`（`app/src/store/db.rs:125`）決定，`LATEST_SCHEMA_VERSION = 4`，`SchemaTooNew` 會停機；啟動只開預設資料庫（`app/src/main.rs:44`）。
- 引擎已有 `simulated` 布林與 `executor_mismatch`（`app/src/engine/actor.rs:1276`）；`IdPrefix` 只有 `Sim`／`Demo`（`app/src/engine/ids.rs`）；可交易清單 `TRADABLE_EXCHANGES = Exchange::ALL`（`app/src/ui/vm/bridge.rs:32`）。
- `live.rs` 啟動時固定綁定 demo 主機（時鐘校時、簽名與流水客戶端、`signed_demo()`），並讀 `TONG_FUNDING_BINANCE_HOST` 環境變數選 demo／testnet。

## Goals / Non-Goals

**Goals:**
- 一個啟動時固定的身分，決定主機、資料庫、鑰匙圈，且互不越界（用測試證明）。
- 即使有人「順手」把 live 主機加進某個清單，demo 程式仍不可能打到 live（傳輸層與型別各擋一次）。
- 不新增 `ExecutionMode` 變體、不改 `simulated` 的語意，避免新增模式時相等比較靜默放行。

**Non-Goals:**
- 武裝流程、硬上限、自動 kill switch（`live-guardrails`）。
- 橫幅、標題、確認對話框文字（`live-banners`）。
- 金鑰權限 API、手續費 API、每日對帳、損失上限、IOC 限價單。
- 重新設計 demo 流程；Demo 身分的行為不變。

## Decisions

1. **身分是一個值，不是模式。** `ProcessIdentity { Demo, Live }` 在 `main` 解析參數後建立一次，往下以參數傳遞（組合根），不放全域可變狀態。`ExecutionMode`（`SIMULATION`／`EXCHANGE_DEMO`）維持二態；Live 身分下 `SIMULATION` 就是影子模式，`EXCHANGE_DEMO`（內部名稱）代表「對該身分的交易所下真單」。顯示名稱由身分決定（`live-banners`）。替代方案：新增第三個 `ExecutionMode`——被否決（審查指出會讓 `gate.rs:59`、`leverage_cap.rs` 等相等比較靜默放行，且重啟與解除武裝會出現平不掉的部位）。
2. **live 主機用獨立型別，不加進 `BinanceHost`／`BybitHost`。** 新增 `LiveHost`（Binance／Bybit 各一個值，`base_url()` 在 `endpoints.rs`），只能由 `ProcessIdentity::Live` 取得；`BinanceHost` 與 `BybitHost` 不出現 `Live` 變體，避免 `TONG_FUNDING_BINANCE_HOST=live` 這種入口。
3. **傳輸層兩道防線**：(a) `HostPolicy::SignedLive` 與 `LIVE_SIGNED_HOSTS` 常數；(b) 傳輸型別區分 `PublicTransport`／`SignedDemoTransport`／`SignedLiveTransport`（薄包裝），簽名客戶端的建構子只接受對應型別。替代方案：只靠執行期政策——被否決（`ui/live.rs` 目前就有把公開傳輸傳給簽名客戶端的先例，OKX 的 resync；加了 live 之後那個接錯就是真錢請求）。
4. **公開政策拒絕所有認證標頭與簽名參數**：`X-MBX-APIKEY`、`X-BAPI-API-KEY`、`X-BAPI-SIGN`、`OK-ACCESS-*`，以及 URL 查詢中的 `signature`、`sign`。
5. **主機字串單一來源**：`PRODUCTION_HOSTS` 掃描擴大為整個 `src`（非測試程式碼），只放行 `signed/endpoints.rs` 與公開客戶端；`HostPolicy::SignedLive` 的清單與 `endpoints.rs` 的常數同源，並加一個測試確認兩者一致。
6. **限頻分類改依客戶端型別**：`RateLimiter` 的呼叫端（簽名客戶端／公開批次／公開單一）直接傳入分類，`classify_request` 只保留給無法得知型別的舊呼叫（逐步移除並有測試覆蓋）。平倉專用的權重保留與 418／429 自動鎖定屬於 `live-guardrails`，本變更只確保分類正確。
7. **資料庫環境中繼資料**：寫入 `system_flags`（或獨立的 meta 表）一個唯讀語意的 `db_venue`，schema 提升到 5；開檔順序：開檔 → 讀 schema 版本（過新即 `SchemaTooNew`）→ 讀 `db_venue` → 與身分比對（不符即新的 `HaltReason::VenueMismatch`）。路徑：Live → `funding-live.db`，Demo → `funding.db`，同一個資料目錄。`config`／`import` 等命令列工具走同一個開檔函式，因此 `--db` 也受約束。
8. **鑰匙圈**：`BundleSecrets` 依身分選 `KeychainStore::with_service("tong-funding")` 或 `with_service("tong-funding-live")`；Live 不做舊分項遷移、不退回 demo 項目；`secrets_cli` 的 `--live` 旗標選擇目標，`import-env` 與 `--live` 互斥。
9. **單一實例鎖**：在資料目錄建立 `tong-funding.lock`，以作業系統的檔案鎖（程序結束或當機自動釋放）取得；取不到就在開資料庫與啟動引擎之前結束。
10. **客戶端依身分選主機**：`live.rs` 的組合根依 `ProcessIdentity` 建立一組「環境套件」（時鐘校時主機、帳戶／流水／費率／槓桿客戶端、下單傳輸）；Live 套件只含 live 項目，Demo 套件只含 demo 項目；用假傳輸記錄所有目標主機的測試證明兩邊不交叉。環境變數 `TONG_FUNDING_BINANCE_HOST` 只對 Demo 身分有效。
11. **`IdPrefix::Live`**：字串 `live`，與 `demo` 等長（4 個字元），總長度維持在 36 以內；`recovery` 以前綴辨識 live 單。
12. **OKX**：Live 身分下 `TRADABLE_EXCHANGES` 只含 Binance、Bybit；`add_prepared` 與 `start_entry` 都檢查腿的交易所；執行器工廠 Live 版不建 OKX 客戶端。

## Risks / Trade-offs

- [改簽名客戶端的建構子型別會影響很多呼叫點與測試] → 以薄包裝型別與機械式改動為主，逐批提交；Demo 行為不變，由既有測試保護。
- [`SchemaTooNew` 讓舊版二進位無法開升級後的資料庫（單向）] → 這是想要的行為（防止舊版把 live 資料庫當 demo 讀）；升級前自動備份 `funding.db`（沿用既有備份慣例，若沒有就在任務中補）。
- [單一實例鎖讓使用者不能同時開 demo 與 live 視窗] → 設計上就是要避免；訊息要清楚。
- [live 主機與公開行情是同一台，Binance 的 IP 權重由掃幣與下單共用] → 本變更只做正確分類；降低掃幣頻率、保留平倉權重、418 自動鎖定在 `live-guardrails`。
- [`TONG_FUNDING_BINANCE_HOST` 這類既有入口] → 只保留給 Demo 身分，並加測試證明 Live 身分忽略它。
- [所有交易所端點與錯誤碼未經網路驗證] → 本變更不新增任何新的交易所端點呼叫，只換主機。

## Migration Plan

1. 先寫失敗的測試（身分解析、政策、靜態檢查、資料庫綁定、鑰匙圈、客戶端主機），再逐批實作。
2. 部署：Demo 使用者首次啟動會升級 `funding.db`（schema 5，加上環境中繼資料），行為不變。
3. 回滾：還原 commit；已升級的資料庫需要用升級前的備份還原（舊版程式會拒絕新資料庫）。

## Open Questions

- 單一實例鎖放在資料目錄還是系統暫存目錄？（預設資料目錄，與資料庫同處，便於使用者理解。）
- 升級前備份：既有的備份機制是否涵蓋 schema 升級？（實作時先確認，不夠就補。）
