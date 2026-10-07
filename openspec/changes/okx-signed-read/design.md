## Context

### 現況（程式證據）

| 能力 | Binance | Bybit | OKX 現況 | 證據 |
|---|---|---|---|---|
| 公開行情（funding、mark、tickers、instruments、refetch） | 有 | 有 | **有** | `exchange/public/okx.rs`、`public/endpoints.rs:15-20` |
| 校時 | 簽名主機 | 簽名主機 | 有（打公開主機 `/api/v5/public/time`） | `health/clock_sync.rs:19,73`、`ui/live.rs:407-421` |
| 限流代碼 | `-1003` | `10006/10018` | 有（`50011/50013`；其中 50013 依文件是「系統忙碌」，見風險） | `health/ratelimit.rs:82-88` |
| 金鑰儲存 | key + secret | key + secret | **有**（含 passphrase） | `store/secrets.rs:101-104`、`secrets_cli.rs:157-159` |
| `Credentials` 帶 passphrase | — | — | **缺**（`load_credentials` 只檢查存在，不保存） | `signed/signing.rs:34-37,117-129` |
| 簽名唯讀客戶端 | `signed/binance.rs` | `signed/bybit.rs` | **缺**（無 `signed/okx.rs`；`OkxAccount` 一律不支援） | `signed/models.rs:91-118` |
| engine `AccountView` | 有 | 有 | **缺**（`OKX_ACCOUNT_UNSUPPORTED`） | `execution/account.rs:18,78,95,105` |
| demo 主機白名單 | testnet / demo-fapi | api-demo | **缺**（OKX 沒有獨立 demo 主機） | `signed/endpoints.rs:21-26`、`reqwest_transport.rs:33-40` |
| 帳戶輪詢 / `LegAccount` | 有 | 有 | **缺** | `ui/live.rs:458-466,672-680` |

### OKX API v5 事實（官方文件 https://www.okx.com/docs-v5/en/ ，2026-10-07 取得）

- **Overview → Production / Demo Trading Services**：正式與 demo 的 REST 主機**都是** `https://openapi.okx.com`；demo 請求須帶 `x-simulated-trading: 1`；demo API key 在「Demo Trading → Personal Center → Demo Trading API」建立；demo key 不會因閒置過期。
- **Overview → Regional API Domain Requirement**：EEA 帳戶須用 `eea.okx.com`、US/AU 帳戶須用 `us.okx.com`，`openapi.okx.com` 對這些地區無效。
- **REST Authentication → Making Requests**：`OK-ACCESS-KEY`、`OK-ACCESS-SIGN`（Base64）、`OK-ACCESS-TIMESTAMP`（ISO 8601 UTC 毫秒，如 `2020-12-08T09:08:57.715Z`；與伺服器差超過 30 秒回 `50102`；建議先以 `GET /api/v5/public/time` 校時）、`OK-ACCESS-PASSPHRASE`。
- **REST Authentication → Signature**：pre-hash = `timestamp + method(大寫) + requestPath + body`；GET 的 query 算在 requestPath 內、body 省略；HMAC-SHA256 後 Base64。
- **Error Codes**：`50101` APIKey 與目前環境不符（demo key 打正式、或正式 key 帶模擬標頭）、`50103/50104` 缺 KEY / PASSPHRASE 標頭、`50105` PASSPHRASE 錯誤、`50111` 無效 KEY、`50113` 簽名無效、`50102` 時間戳過期、`50011` 限流、`50061` 子帳戶下單限流、`50001/50004/50013/50026` 服務不可用 / 逾時 / 忙碌 / 系統錯誤。
- **Account → Get balance**（`GET /api/v5/account/balance`，10 次 / 2 秒，User ID）：帳戶層 `totalEq`、`adjEq`、`availEq`（「Applicable to Multi-currency margin / Portfolio margin」）；`details[]` 每幣 `ccy`、`eq`、`cashBal`、`availBal`、`availEq`（「Applicable to Futures mode / Multi-currency margin / Portfolio margin」）、`eqUsd`。
- **Account → Get account configuration**（`GET /api/v5/account/config`）：`acctLv` 1 現貨 / 2 合約模式 / 3 跨幣種保證金 / 4 組合保證金；`posMode` `long_short_mode` / `net_mode`。
- **Account → Get positions**（`GET /api/v5/account/positions`，10 次 / 2 秒）：`pos` 單位為**張數**（SWAP），net 模式下正為多、負為空；`posSide` `net/long/short`；`mgnMode` `cross/isolated`；`avgPx`、`markPx`、`upl`、`lever`、`imr`、`notionalUsd`。
- **Trade → Get order List**（`GET /api/v5/trade/orders-pending`，60 次 / 2 秒）：只回 `live` 與 `partially_filled`；`after`（ordId）分頁、`limit` 最大 100；`sz`、`accFillSz` 對 SWAP 為張數。

### 限制

- 程式現有的安全模型是「簽名請求只能抵達 demo/testnet 專用主機」（`signed-read-access` 第一條 requirement、`reqwest_transport.rs` `HostPolicy::SignedDemo`、`static_checks.rs` 的正式主機掃描）。OKX 沒有 demo 專用主機，這條防線對 OKX 不成立，必須以等價機制取代。
- agent 不得讀寫真實 Keychain、不得以真實金鑰發出請求；所有測試以錄製回應與假傳輸層進行，實機步驟由使用者執行。

## Goals / Non-Goals

**Goals:**
- OKX demo 帳戶的餘額、可用保證金、持倉、未成交委託可被安全地讀取，失敗時明確（未連線 / 不完整 / 模式不支援），不當成空資料或 0。
- 讓 OKX「不可能」誤送正式環境的簽名請求：標頭在建構時寫死、傳輸層二次檢查、靜態檢查鎖定。
- engine 的 `AccountView` 對 OKX 回真實資料（為後續下單、平倉、FINALIZED 檢查鋪路）。

**Non-Goals:**
- 下單 / 撤單 / 查單（`okx-demo-execution`）、流水（`okx-funding-ledger`）、頁面可下單與 OKX 卡片 / 持倉列（`okx-trading-enablement`）。
- 組合保證金（`acctLv` 4）、現貨模式（1）、long/short 持倉模式、逐倉、自動切換帳戶設定。
- EEA / US 網域、WebSocket 私有頻道、正式（真錢）環境。
- 把公開行情主機從 `www.okx.com` 改為 `openapi.okx.com`（見 Open Questions 2）。

## Decisions

**D1　OKX demo 邊界 = 寫死主機 + 建構即帶模擬標頭 + 傳輸層檢查 + 靜態檢查。**
`signed::endpoints` 新增 `OkxHost::Demo`（主機 `openapi.okx.com`）並加入 `ALLOWED_SIGNED_HOSTS`。`OkxHost` 不提供單獨取得主機或基底網址的方法，唯一的 API `OkxHost::target()` 同時回傳基底網址與模擬交易標頭（標頭名稱與值是同檔的常數）；signed 的 GET 建構與後續 execution 的下單建構都只能經由它取得網址，並無條件套用回傳的標頭，沒有參數可關閉。`ReqwestTransport`（`HostPolicy::SignedDemo`）與 `ReqwestOrderTransport` 在主機為 OKX 時，若標頭不存在或值不是 `1`，零連線拒絕。靜態檢查：`openapi.okx.com` 字面只准出現在 `signed/endpoints.rs`；`x-simulated-trading` 字面只准出現在 `signed/endpoints.rs` 的常數定義；`signed/okx.rs` 不得出現 `www.okx.com`。
第三道（交易所端）防線（**未驗證，待使用者實機確認**）：依文件 Error Codes 的一行泛稱，環境不符時 OKX 回 `50101`；兩個方向（demo key 打正式、正式 key 帶模擬標頭）是否都如此，是推論而非保證。其中「demo key 缺標頭」我們的程式碼永遠不會送出，只能查文件。
- 替代 A：簽名也用 `www.okx.com`（Python 版做法）。缺點：與公開行情主機相同，`HostPolicy` 與 `ratelimit::classify_request`（以主機判斷 Signed 類別）都無法區分；正式主機掃描必須開洞。
- 替代 B：不做 OKX 簽名（維持現狀）。與本計畫目標衝突。
- 選擇 `openapi.okx.com`：文件 2026-10 版明載為 REST 主機；與公開行情主機字面不同，`HostPolicy` 與限流分類可沿用「以主機判斷」；但它**也是正式主機**，所以安全不靠主機名而靠標頭與 demo key，spec 明寫這點。

**D2　簽名時間戳沿用校時偏移，格式化為 ISO 8601 UTC 毫秒。**
`signed_timestamp`（本機時間 + OKX 偏移）→ `chrono` 格式化 `%Y-%m-%dT%H:%M:%S%.3fZ`。OKX 偏移目前由公開 `/api/v5/public/time` 校時（`live.rs:420`），同一伺服器時間，沿用即可；未校時 → `ClockUnsynced`，不送。`50102` 視為時間戳被拒：重新校時一次、再送一次（與 Binance `-1021`、Bybit `10002` 相同，擴充 `signing::is_timestamp_rejected`）。
- 替代：用本機 UTC 不校時。OKX 容差 30 秒較寬，但與其他兩所一致的「未校時不簽名」較安全。

**D3　`Credentials` 增加 `passphrase: Option<String>`，OKX 一律要求。**
`load_credentials(.., Exchange::Okx, true)` 回傳含 passphrase 的 `Credentials`；`Debug` 不印任何值；passphrase 註冊進 `redact_secrets`。缺少 → `NoPassphrase`（`signing.rs:86` 已預留）。

**D4　帳戶模式閘門：只接受 `acctLv ∈ {2, 3}` 且 `posMode = net_mode`。**
理由：`acctLv` 1 不能交易永續；4（組合保證金）下 `reduceOnly` 不適用（Place order 文件：「Only applicable to Futures mode and Multi-currency margin」），而平倉依賴 reduce-only；`long_short_mode` 需要 `posSide`，與 engine 的單向假設不符（等同 Binance / Bybit 的 hedge 拒絕，`account.rs:34-37`）。讀取結果快取 60 秒（與 `POSITION_MODE_TTL_MS` 相同），不符或讀不到時不快取。
- 替代：支援 long_short 並在送單帶 `posSide`。增加平倉與數量比對的複雜度，且與另外兩所的行為不對等，延後。

**D5　可用保證金：`acctLv` 2 取 `details[ccy=USDT].availEq`；`acctLv` 3 取帳戶層 `availEq`（USD 視同 USDT）。空值、缺欄位即錯誤（fail closed）。**
對齊 `bybit-available-margin` 的決定（取交易所自己算的可開倉額度，不自行計算、不 fallback）。
- 替代：`availBal`。它是現金可用餘額，不含未實現損益，與 Bybit `totalAvailableBalance` 語意不同；只在 D5 欄位缺漏時列為 Open Question，不實作 fallback。

**D6　持倉在兩個層級使用不同單位，型別上分開。**
OKX 客戶端回傳 `OkxPosition { symbol, contracts, pos_side, mgn_mode, avg_px, mark_px, upl, lever, imr, notional_usd }`（`contracts` 為帶號張數）。
- engine：`AccountPosition.quantity` = 張數（`engine/ports.rs:127-135` 既有契約「OKX 為張數」），不需 `ctVal`。
- 頁面：`models::Position.quantity` 定義為 base 幣量（`models.rs:37-39`），轉換時以公開 instruments 的 `ctVal`（`OkxAdapter::instrument_rules`）乘上；`ctVal` 未知 → 該列標示「無法換算」，不猜。
- 出現 `posSide` 為 long/short 或 `mgnMode = isolated` 的持倉 → 整份列表 `Err`（與 hedge 同等處理；逐倉部位不會被系統的全倉 reduce-only 單平掉）。

**D7　未成交委託以 `after` 分頁到不足 `limit` 為止；頁數上限沿用 `MAX_PAGES`；中途失敗 → `Incomplete`。**
OKX 以 ordId 游標分頁（非 Bybit 的 `nextPageCursor`）。重複游標視為不完整。

**D8　回應判讀：HTTP 200 + `code != "0"` → `AdapterError::Exchange { code, msg }`；`50011`/`50061` → `RateLimited`。**
`50013` 在既有 `ratelimit.rs:88` 被當成限流，但文件寫「Systems are busy」；本 change 只在簽名路徑把它當一般交易所錯誤，公開路徑的歸類修正列為 Open Question 3（不在此 change 改，避免影響已驗證的行情輪詢）。

### 與既有 spec 的關係

`signed-read-access`（未封存的 `exchange-readonly-adapters`）第 24 行「OKX SHALL NOT 有任何簽名請求的實作」由本 change 的 `okx-signed-read` 取代；第 3 行「簽名請求只能抵達 demo/testnet 主機」對 OKX 改以 D1 的等價機制滿足。依專案慣例（`bybit-available-margin`、`manual-order-picker` 皆以新 capability 落地），本 change 以新 capability 撰寫；`exchange-readonly-adapters` 封存時，須把上述兩句改為引用 `okx-signed-read`。

## 路線圖

OKX 對等分成五個依序的 change（原四個；`okx-execution-guards` 為抗辯審查後自 `okx-demo-execution` 拆出），每個 ≤ 12 項 task：

| 順序 | change | 內容 | 依賴 |
|---|---|---|---|
| 1 | `okx-signed-read`（本 change） | demo 邊界、簽名、帳戶模式、餘額 / 保證金 / 持倉 / 委託、`AccountView`、輪詢接線 | 無 |
| 2 | `okx-demo-execution` | 送單 / 撤單 / 查單（`clOrdId`、`tdMode=cross`、張數）、結果分類、手續費正負號、執行器與工廠（OKX 金鑰可選）、使用者實機探針 | 1 |
| 2b | `okx-execution-guards` | 實盤前守衛：demo 金鑰正向證明與 `50101` 閂鎖、`lotSz` / `ctVal` / 名目上限、`expTime` 與雙重 `51603`、拒絕碼白名單、平倉模式不快取與裸腿告警、不經 `GatedTransport`；實機探針的 OKX 腿 | 1、2 |
| 3 | `okx-funding-ledger` | 帳單（`type=8`）流水客戶端、`LedgerSource`、OKX 腿的 `ctVal` 寫入成交事件、PnL 不再把 OKX 腿標為未知 | 1、2（OKX 成交事件格式） |
| 4 | `okx-trading-enablement` | 可下單集合納入 OKX、掃幣方向與達標涵蓋三所、候選勾選、手動下單 OKX 面板（輸入幣量換張數）、交易單頁保證金、總覽 OKX 帳戶卡、持倉頁 OKX 列 | 1、2、3；以及另一位 agent 進行中的 `trade-cost-estimate`（OKX 一檔掛單量為張數，見該 change 的風險） |

建議在 4 之前完成 3：否則 OKX 配對可以自動進場，但其 PnL 會一律 INCOMPLETE。

**刻意不做（全計畫）**：真錢 / 正式環境；EEA、US 網域；組合保證金與現貨模式；long/short 持倉模式；逐倉；自動設定槓桿或帳戶模式；OKX WebSocket（私有或公開）；批次下單；公開行情改主機（Open Question 2）。

## Risks / Trade-offs

- [`openapi.okx.com` 也是正式主機，安全不再由主機保證] → D1 的三層：建構即帶標頭（無法省略）、傳輸層零連線拒絕缺標頭的 OKX 請求、OKX 端對環境不符 key 的 `50101`（未驗證的推論，不作為唯一依據）；加上靜態檢查與錄製測試逐一斷言每個 OKX 請求都帶 `x-simulated-trading: 1`。
- [使用者的 OKX 帳戶若註冊在 EEA / US，`openapi.okx.com` 無效] → 實機步驟第一步即確認；不在本計畫加入地區網域（Open Question 1）。
- [文件欄位與 demo 實際回應不同（Python 版從未驗證成功）] → 解析器只依文件、fixtures 標註「依文件構造、未驗證」；實機 task 由使用者以真實 demo 帳戶比對後才把 fixtures 換成去識別化的真實回應。
- [`availEq` 在某些模式為空] → fail closed（保證金檢查不通過、頁面顯示「未知（原因）」），不 fallback。
- [多一個每 15 / 30 秒輪詢的交易所] → OKX 餘額與持倉各 10 次 / 2 秒，委託 60 次 / 2 秒，遠高於輪詢頻率。

### OKX 特有陷阱（本 change 範圍）

- 數量單位：`pos`、`sz`、`accFillSz` 對 SWAP 一律是**張數**；engine 層保留張數，只有頁面換 base 幣量。
- passphrase：建立 API key 時使用者自訂，遺失無法復原；缺少時不得以空字串簽名。
- 時間戳是 ISO 字串而非毫秒整數；簽名的 requestPath 須含 `?query`，且與實際送出的字串逐字相同。
- 帳戶模式與持倉模式是帳戶層級設定，只能在網頁 / App 首次設定，系統只讀不改。
- demo key 與正式 key 互不相通（`50101`）；demo key 不會因閒置過期。

## Migration Plan

- 純新增；OKX 從「不支援」變為「未連線（缺金鑰）」或真實資料。沒有 OKX 金鑰的使用者看到的是「OKX 未連線：NoKey」，而非原本的「僅比價」——頁面文案的調整在 `okx-trading-enablement`；本 change 期間 `dashboard.rs:245` 的 OKX 說明卡維持不動（頁面不讀 OKX 帳戶狀態）。
- 回復：移除 OKX 分支即恢復 `Unsupported`；無資料格式變更。

## Open Questions

1. 使用者的 OKX 帳戶註冊地區（決定主機是否為 `openapi.okx.com`）？
2. 公開行情是否一併改用 `openapi.okx.com`？（目前 `www.okx.com` 仍可用；不在本計畫內改。）
3. `50013` 在公開路徑是否應改為「暫時錯誤」而非限流？
4. demo 帳戶的 `acctLv` 預設值為何（使用者實機回報）；若為 1（現貨模式）需使用者手動切換為合約模式。

## 實作時發現

- `Exchange::Okx` 的「帳戶模式不支援」沒有新增 `AdapterError` 變體（`feed.rs`、`classify.rs` 有窮舉 match），以 `Exchange { code: "acct_mode_unsupported", message: "帳戶模式不支援 (acctLv X, posMode Y)" }` 表示。
- OKX 認證失敗可能帶 4xx 狀態與 JSON `code`：`attempt()` 對非 2xx（429 除外）先嘗試解析 `code`，使 `50102` 在 401 下仍會重校時重送；無法解析才回 `Http{status}`。
- 公開策略（`HostPolicy::PublicProduction`）對 `www.okx.com` 不套用「必須有模擬標頭」：公開行情是無簽名請求；改為拒絕任何帶 `x-simulated-trading` 或 `OK-ACCESS-*` 的公開請求。「`*.okx.com` 必須帶標頭」只適用簽名策略。
- 公開層與簽名層共用唯一的 `okx_inst_id` / `okx_symbol`（`signed/endpoints.rs`），`public/okx.rs` 的私有副本已刪除。
- 模式快取維持在 `OkxSignedClient`（60 秒，沿用 `POSITION_MODE_TTL_MS`）：本 change 沒有 executor，spec 情境「60 秒內不再重讀帳戶設定」要求讀取端自己快取；`okx-demo-execution` 的 executor 對 OKX 直接呼叫同一個 `account_mode()`，不另建第二份快取（抗辯 #6）。

## 抗辯修正

1. `HostPolicy::allows(url, headers)` 是唯一的准入規則，host 與 OKX 標頭一起判斷，`reqwest_transport.rs` 與 `execution/http.rs` 皆呼叫它。
2. 簽名策略下，任何 `*.okx.com` 主機須恰有一個 `x-simulated-trading`（名稱不分大小寫）且值恰為 `1`；零個、重複（含不同大小寫）、其他值皆零連線拒絕。
3. `openapi.okx.com` 列入 `PRODUCTION_HOSTS`（只有 `signed/endpoints.rs` 可豁免）；字面規則涵蓋整個 crate 含 `ui/live.rs`。
4. `50101` 的雙向拒絕標為「未驗證，待使用者實機確認」（見上方 D1 與 tasks 4.2）。
5. 50102 重試重新經 `HttpRequest::okx_signed_get` 建構；測試斷言重送的請求仍帶標頭，且經真實 `HostPolicy` 准入。
6. 模式閘門：單一 60 秒快取，常數沿用 `POSITION_MODE_TTL_MS`（見上「實作時發現」）。
7. `okx_inst_id` / `okx_symbol` 單一配對（`signed/endpoints.rs`）。
8. clOrdId 長度改為單元測試（屬 `okx-demo-execution`）。
9. 原 3.2 / 3.3 移至 `okx-trading-enablement` 3.4 / 3.5。
A. `HttpRequest::header()` 拒絕 `x-simulated-trading` 與 `OK-ACCESS-*`（丟棄並標記，傳輸層零連線拒絕）；OKX 請求只能由 `HttpRequest::okx_signed_get` 建構，旗標由它在私有欄位序列中先插入。傳輸層對「任何 `OK-ACCESS-*` 請求」也要求旗標。
B. 靜態規則（`static_checks.rs`，整個 `src/` 與 `tests/`）：`reqwest` 識別字只准在 `reqwest_transport.rs` 與 `execution/http.rs`；`x-simulated-trading` 字面只准 `signed/endpoints.rs`；`OK-ACCESS` 只准 `signed/endpoints.rs` 與 `signed/signing.rs`；`okx.com` 只准 `signed/endpoints.rs` 與 `public/endpoints.rs`；各有反例測試，且與 `host_fragments`（豁免白名單主機）分開。
C. 兩個真實客戶端皆 `.no_proxy()`、無重導向、無自動重試；測試以 `HTTP_PROXY` 驗證（含對照組證明測試非空轉）。
D. 以本機假伺服器 + `LocalOkxTest` 策略證明缺標頭 / 重複 / `header()` 嘗試皆零連線；POST 版本在 `okx-demo-execution`。

## 抗辯修正（第二輪）

- S2：`.no_proxy()` 是**刻意的行為變更，且同時影響 Binance / Bybit** 的簽名客戶端與下單客戶端：簽名與下單流量不得經過會攔截的環境代理（`HTTP(S)_PROXY`）。啟動時若設有代理變數，`ui/live.rs` 印一行警告（`proxy_env_warning`，只列變數名，不印值）說明簽名 / 下單流量會忽略它。以 `HTTP_PROXY` 測試鎖定（含對照組）。
- S3：以**真實** `ReqwestTransport::get` 與 `ReqwestOrderTransport::send`（`signed_demo()` 策略）測試：真實 OKX 主機缺旗標、重複、值 0、`header()` 嘗試、只有 `OK-ACCESS-*` 皆在任何 DNS / 連線之前被拒絕（錯誤等於策略拒絕 `host not allowed`、耗時 < 500 ms）；測試從不送出會通過的請求。
- R7：`FakeOrderTransport::send` 對每個 OKX 請求斷言真實的 `HostPolicy::SignedDemo.allows`（違規即 panic），executor 層測試因此跑真實的准入規則。
- R8：`header()`（`HttpRequest` 與 `OrderHttpRequest`）拒絕不是合法 HTTP token 的名稱（`http::HeaderName::from_bytes`：尾端空白、換行、unicode 相似字…），標記 `protected_header_misuse`；新增直接依賴 `http`（已在 lockfile）。
- 簡化：`POSITION_MODE_TTL_MS` 移到簽名層（`signed::okx`，executor 重新匯出）；OKX 速率 / 未知碼只有一份 `i64` 清單（`signed::okx`，下單路徑共用）；`bills_page` 直接是 `OkxSignedClient` 的方法，不再有 `OkxLedgerClient` / `get_signed`；`DemoAccountView` 一律用 `is_complete()`。未做（列為後續簡化，無行為影響）：body 只解析一次的 `EnvMismatch` 變體、executor 每次呼叫只綁定一次 OKX client（`Result<OkxOrderClient, String>`）、共用 `#[cfg(test)] test_support` 模組、少數重複測試合併。
