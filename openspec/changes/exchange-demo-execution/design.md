## Context

來源：`mvp-python`（HANDOFF.md 為準）、SYSTEM_SPEC §20–§30、2026-10-05 抗辯與使用者拍板（單腿失敗完全人工）。
`engine-simulation` 已定義 `Executor`（送單、撤單、依 `client_order_id` 查單）與 `AccountView`（持倉、委託、餘額）介面、重啟對帳與警示契約；本 change 提供真實實作與其驗證。

**已對照原始碼確認的 Python 版事實**

| 事實 | 來源 |
|---|---|
| 兩腿送單是 `for` 迴圈依序執行，不是並行 | `trade_pipeline.py:submit_legs` |
| `submit_legs` 只捕捉 `BinanceApiError`、`BybitApiError`、`OkxApiError`；`requests` 的逾時與連線例外不被捕捉，會往上拋，使配對停在 `ORDER_SUBMIT`，已成功那腿的訂單仍在交易所 | `trade_pipeline.py`、`auto_scheduler.py:_run_forever` |
| 兩支 client 的 `place_market_order` 都只送 symbol、side、type、quantity，沒有 client order id、沒有 reduce-only | `binance_client.py`、`bybit_client.py` |
| 平倉數量取自配對記錄的 `long_qty` / `short_qty`（只經 lot size 取整），不是交易所持倉；方向由配對記錄的 long/short 推斷 | `trade_pipeline.py:submit_legs`（`closing=True`） |
| `confirm_fills` 只查「該標的是否存在非零持倉」且與目標差在 20% 內；查詢發生 `*ApiError` 時 `pass`，等同未確認 | `trade_pipeline.py:confirm_fills` |
| 下單只用市價單（`MARKET` / `Market`） | 兩支 client |
| HTTP 逾時 10 秒；Bybit 的 `retCode` 非 0 即丟 `BybitApiError` | 兩支 client |
| Binance 預設 `https://testnet.binancefuture.com`，`.env.example` 註記若授權失敗可改試 `https://demo-fapi.binance.com`；Bybit 預設 `https://api-demo.bybit.com`；OKX demo 與正式同主機、靠 header 區分 | `app.py`、`.env.example`、`okx_client.py` |
| OKX 簽名端點從未在真實 demo 帳戶驗證成功 | HANDOFF Fragility #6 |
| `order_timeout_seconds` 沒有任何執行路徑讀取 | HANDOFF Fragility #1 |

**我在 2026-10-05 對公開、不需簽名的端點做的唯讀查證**

| 查證 | 結果 |
|---|---|
| `GET https://fapi.binance.com/fapi/v1/exchangeInfo` 的 `rateLimits` | `REQUEST_WEIGHT` 2400／分、`ORDERS` 1200／分、`ORDERS` 300／10 秒（正式主機，僅供參考） |
| `GET https://testnet.binancefuture.com/fapi/v1/exchangeInfo` 的 `rateLimits` | `REQUEST_WEIGHT` **6000**／分、`ORDERS` 1200／分、`ORDERS` 300／10 秒（與正式主機的權重上限不同） |
| `GET https://demo-fapi.binance.com/fapi/v1/time` | 回應 `serverTime`（公開端點可連線） |
| `GET https://api-demo.bybit.com/v5/market/time` | 回應成功（公開端點可連線） |
| Bybit 各端點的限流數值 | **未查證** |

以上只證明公開端點可連線與上述數值在查證當下成立，**不證明哪個 Binance 主機接受使用者的金鑰**，也不證明簽名端點的限流與公開端點相同。實作須從 `exchangeInfo` 讀取限流而非寫死數值。

**Python 版事件統計與我的唯讀檢視（`data/events.jsonl`，2026-10-05）**
`ORDER_SUBMIT_FAILED` 48 筆：46 筆為 Bybit 進場，錯誤本文為字面上的 `Bybit API error 400: rejected`；1 筆 Binance 進場 `{"code":-2027,"msg":"Exceeded the maximum allowable position at current leverage."}`；
1 筆 Bybit 平倉 `retCode 110090`（訊息為「持倉與委託合計已達目前風險等級上限，只能 reduce-only 或平倉」）。
`PAIR_PARTIAL_FAILURE` 47 筆中 46 筆是「Binance 成功、Bybit `rejected`」，另 1 筆 Binance `-2027`；`PAIR_CLOSE_PARTIAL_FAILURE` 1 筆（Bybit 110090）。時間範圍 2026-10-04 13:16 至 2026-10-05 14:46。
那 46 筆 `rejected` 不像交易所真實回應（Bybit 真實回應是 JSON），HANDOFF 指出 `logger.LOG_PATH` 沒有被測試自動隔離，因此**可能是測試替身寫進了真實日誌，但未驗證**；`PAIR_FINALIZED` 的 48 筆是否也受影響同樣未驗證。
這表示 Python 版數字不能當作單腿失敗頻率的基準；另外兩筆真實失敗（持倉上限、風險等級上限）說明失敗原因確實多與交易所帳戶限制有關。

## Goals / Non-Goals

**Goals**
- 在 Binance 與 Bybit 的 demo/testnet 帳戶上真實下單、撤單、查單、平倉，且每一步都有明確的結果分類。
- 單腿失敗、部分成交、結果未知都進入明確狀態並警示，成功那腿資料不丟，系統零自動動倉位。
- 取得可重現的真實驗證紀錄與警示頻率數據。

**Non-Goals**
- 不做真錢；不存在真錢端點。
- 不做 OKX 簽名端點與下單。
- 不做自動補平衡、自動重試、自動平倉（SYSTEM_SPEC §27 的自動處理不採用）。
- 不做限價單、不做訂單簿深度判斷、不做 WebSocket 私有頻道（成交以輪詢查單，見 D6）。
- 不做 Funding PnL。

## Decisions

**D1　`Executor` 的真實實作分交易所，共用一個可替換的傳輸層。**
`BinanceExecutor`、`BybitExecutor` 各自負責簽名與參數對應，共用 `Transport`（真實為 HTTP，測試為錄製回應）。理由：結果分類、限流、遮蔽都在共用層做一次；錄製回應的測試不需金鑰。

**D2　端點是編譯期常數，沒有覆寫途徑。**
Python 版靠環境變數 `BINANCE_BASE_URL` 切換主機，設定錯誤就能指向任何主機。新版把主機寫成常數並以允許清單測試守住，使真錢在結構上做不到。代價：Binance 究竟用 `testnet.binancefuture.com` 還是 `demo-fapi.binance.com` 須先在 4.2 實測，實測前不能定案（Open Questions 1）。

**D3　`client_order_id` 由引擎產生並原樣傳遞。**
執行器只負責放進 `newClientOrderId` / `orderLinkId`，不產生也不修改。這個欄位名稱與「兩支 client 現在都沒帶」是已知事實；**各所對長度與字元集的限制、重複 id 的回應、以及是否能依此 id 查到舊單，皆未驗證**，4.2 實測。

**D4　送單結果四分類，未知結果必經查單。**

| 分類 | 條件 | 意圖狀態 |
|---|---|---|
| 已接受 | 交易所回報 order id | 已確認 |
| 已拒絕 | 交易所明確回覆拒單（HTTP 4xx 帶錯誤碼；Bybit HTTP 200 且 `retCode` 非 0） | 失敗（確定沒有訂單） |
| 被限流 | HTTP 429（與 418 的處理待定） | 先查單確認未成立才標失敗 |
| 結果未知 | 逾時、連線中斷、回應無法解析 | 已送出、結果未知；查單後才定案 |

429 是否保證「請求未被處理」**未驗證**，所以不直接標失敗；一律查單確認。這直接修正 Python 版 `requests` 例外被吞掉的缺陷。

**D5　兩腿並行（`join`），不是依序。**
依序送單使第二腿延遲第一腿的往返時間，增加單腿失敗與價差漂移的機會。並行後兩腿各自分類，仍可能出現一腿接受、一腿未知，這由 D4 與警示處理，不由並行與否決定。

**D6　成交確認以輪詢查單，間隔暫定 500 毫秒，輪詢共用限流器。**
500 毫秒是**我的提議，未驗證**，需對照 Binance 的 `ORDERS` 與權重上限及 Bybit 的限制（未查證）後在 4.2 調整。私有 WebSocket 頻道（成交推送）較即時但增加連線管理與斷線處理，列為後續選項而非本 change 範圍。

**D7　逾時：撤銷自己未成交的單，再查最終成交量，再呼叫 `next()`。**
逾時自 `request_sent_at` 最早者起算，使用生效的 `order_timeout_seconds`。撤單與成交可能競爭，所以撤單後必須重查，並以「最終成交量」而非逾時當下的值分流。撤單結果與最終狀態任一無法確認就是 `UNRESOLVED`，不猜。起算點是我的選擇（Open Questions 5）。

**D8　警示由狀態推導，通知是附加。**
常駐警示讀自 store 內配對狀態，重啟後自然還原；系統通知只是一次性的附加通道，失敗不影響前者。macOS 通知的實作機制（呼叫 `osascript`，或使用系統通知框架——後者可能需要有簽署的 app bundle）**未驗證**，在 task 3.1 實機試驗後決定並記錄。

**D9　平倉：先查持倉，reduce-only，方向看持倉正負。**
Python 版用配對記錄的數量與方向，若成交量有偏差、被強平或使用者手動動過倉位就會平錯。新版每次平倉前重新查詢，數量走 `Quantity::from_exchange_position`。reduce-only 的**參數名稱與在不同持倉模式下的行為，我沒有查證**（Python 版從未使用；僅在 Bybit 的 110090 訊息文字中看到 reduce-only 的概念），須在 4.2 實測，並且持倉模式（單向或雙向）須在送單前確認（`signed-order-execution` 要求）。我印象中某些交易所在雙向持倉下對 reduce-only 有不同行為，**此為未驗證的記憶，不可當作事實**。

**D10　FINALIZED 確認含重試。**
持倉與委託的更新可能延遲，所以確認在 `order_timeout_seconds` 內重試；逾時仍非零或無法查詢，轉 `PARTIAL_FAILURE`，不假設已平倉。「無未成交委託」取該標的在兩所的全部未成交委託（不只自己的），這是保守選擇：使用者另有的同標的委託也會使確認失敗，但可避免遺漏。

**D11　不平衡以幣量計，公式 `|L − S| ÷ max(L, S) × 100`。**
核心是兩腿的 delta 是否抵銷，所以用幣量而非名目本金；SYSTEM_SPEC §28 另列 imbalance notional 與 hedge ratio，本 change 只做前者並把兩腿成交量寫進事件，使後續可重算。這是我的定義，core 的 spec 沒有給公式（Open Questions 6）。

**D12　驗證分兩層：錄製回應（自動）與真實 demo（使用者在場）。**
錄製回應涵蓋逾時、拒單、部分成交、429，這些在真實 demo 難以安全重現。真實 demo 涵蓋「交易所真的接受請求」：host、id 格式、reduce-only、持倉模式、小量開平倉與兩個時點的崩潰重啟。驗證腳本只能對寫死的 demo/testnet 常數運作，且需要使用者在場確認每筆訂單；代理不得自行使用金鑰。

## 與 Python 版的差異

| 項目 | Python 版 | Rust 版 | 原因 |
|---|---|---|---|
| client order id | 無 | 每單必帶 | 崩潰恢復與查單 |
| 送單 | 依序 | 並行 | D5 |
| `requests` 例外 | 未捕捉，配對卡住 | 「結果未知」並查單 | D4 |
| 成交確認 | 有無持倉、容差 20%、錯誤即未確認 | 依 order id 查單、錯誤為結果未知 | 已確認缺陷 |
| 平倉數量與方向 | 配對記錄 | 交易所持倉與其正負 | D9、使用者決定 |
| reduce-only | 無 | 必帶 | D9 |
| `order_timeout_seconds` | 未使用 | 生效值 | 使用者決定 |
| 單腿失敗 | `PARTIAL_FAILURE` 僅記錄 | 常駐警示 + 系統通知 + 事件，人工出口須驗證 | 使用者決定 |
| 主機 | 環境變數可改 | 編譯期常數 | D2 |
| FINALIZED | 平倉單送出成功即 FINALIZED | 持倉為 0 且無委託 | SYSTEM_SPEC §29 |

## Risks / Trade-offs

- **完全人工的警示頻率可能很高。** demo/testnet 的流動性與帳戶限制（例如已見的 `-2027`、`110090`）可能使單腿失敗頻繁，且每次都需要人處理。Python 版的 47 筆不能直接外推（見 Context）。4.3 量測後由使用者決定是否接受；若不可接受，選項（例如失敗達 N 次自動啟用 kill switch）屬於行為變更，須使用者另行決定，本 change 不預設。
- **demo/testnet 行為可能與實盤不同**（流動性、撮合、限流）。通過 demo 驗證不代表交易所端行為一致；這是設計上接受的限制，因為不做真錢。
- **並行送單放大「兩腿同時未知」的機率。** 兩腿都逾時時，配對依賴查單才能定案；查單也失敗就是 `UNRESOLVED`，需要人工到交易所確認。
- **reduce-only 與持倉模式為未驗證假設。** 若實測發現行為與預期不同，平倉流程與持倉模式檢查需要調整，可能影響 task 1.1、3.3。
- **撤單與成交的競爭窗口**無法消除，只能以「撤後重查」降低誤判。
- **限流數值與間隔 500 毫秒為未驗證／暫定。** 兩腿並行加輪詢在多配對同時進場時可能觸及 Binance `ORDERS` 的 10 秒上限（300，正式與 testnet 查證值相同）；Bybit 限制未查證。

## Open Questions

1. **Binance demo 主機：`testnet.binancefuture.com` 或 `demo-fapi.binance.com`？** 兩者公開端點皆可連線；哪個接受使用者的金鑰須在 4.2 實測，之後才能把常數定案。
2. **reduce-only 的參數名稱、`client_order_id` 的長度與字元集、重複 id 的回應、持倉模式的查詢方式與雙向持倉下的行為**，全部未驗證，由 4.2 實測並回寫此文件。
3. **Python 版 46 筆 `rejected` 是否為測試汙染。** 若是，單腿失敗的真實基準幾乎沒有資料；若不是，Bybit demo 對進場有系統性拒單，會直接影響本 change 的可用性。
4. **平倉時實際持倉與配對記錄不符的處理。** 決策是以實際持倉平倉；但若實際持倉大於記錄（例如使用者在同標的另有持倉），會一併平掉不屬於本配對的部位。目前只記事件。是否在差異超過某比例時要求人工確認，未決。
5. **逾時起算點。** 目前自最早 `request_sent_at` 起算；替代是自最後一腿 ACK 起算。
6. **不平衡公式（D11）**由我定義，core spec 未規定；需使用者確認幣量或名目。
7. **core 需補的轉移**：`CLOSING → PARTIAL_FAILURE`、`FILL_MONITOR → IMBALANCED`（`core-domain-and-fixtures` spec 列有 `IMBALANCED` 狀態，但轉移條件未明列）、人工確認已平倉後的目標狀態；我只讀了 spec，未檢查 core 程式碼，本 change 不得修改其他 change。
8. **`exchange-readonly-adapters` 的持倉需以正負號表示方向。** Bybit 持倉的 `size` 無正負，方向在 `side` 欄位；`Quantity::from_exchange_position` 以正負號判斷方向，需 adapter 先正規化。
9. **HTTP 429 與 418 對訂單請求的語意**（是否保證未處理）未驗證，現以「查單確認」規避。
10. **只支援市價單**（沿用 Python 版）。市價單在流動性差的 testnet 可能滑價大或部分成交；是否需要限價選項未決。
11. **macOS 系統通知的實作機制**未驗證（D8）。
12. **完全人工的負擔門檻**由使用者在 4.3 後判斷；系統不預設任何自動停機規則。

## 決定紀錄（2026-10-05 晚，使用者）

- **只支援市價單**（Open Question 10）。
- **平倉數量 = min(配對記錄量, 實際持倉)**，差異超過 `max_leg_imbalance_pct` 轉人工（Open Question 4），與 `engine-simulation` 一致。
- **成交明細（價格、數量、手續費、手續費幣別）寫入不可變事件**（`funding-pnl` Open Question 2）。
- 已在 `engine-simulation` 處理：core 的 `CLOSING → PARTIAL_FAILURE`、`FILL_MONITOR → IMBALANCED` 轉移已存在（Open Question 7）；不平衡量為幣本位相對差（對較大者，Open Question 6）。
- 金鑰只放 macOS Keychain；雲端開發環境連不到交易所，真實 demo 驗證（4.2）一律在使用者的 Mac 上執行。

## 實作時的決定（2026-10-05，tasks 1.1–1.4、2.1–2.2、3.1–3.3、4.1）

以下為實作時須自行決定之處，一律取保守選項；標「未驗證」者在 4.2 實測後回寫。

**模組與靜態檢查**
- 下單程式只在 `app/src/exchange/execution/**`。原本 `static_checks` 的「簽名請求只能 GET」規則**有意地**演進為：POST / PUT / DELETE / PATCH 只准出現在 `execution/`（`non_get_methods_only_in_execution`：`exchange/` 其他檔以簽名客戶端的完整清單掃描，crate 其餘部分掃 HTTP method 寫法）；`execution/` 必須通過簽名客戶端的其餘全部規則（無正式主機、無主機參數或公開主機欄位、不讀環境變數或設定檔、不用可組出主機的巨集或跳脫、不引用 `public`、只允許完整的 `#[cfg(test)] mod`）；`execution/` 的字串常值不得含任何網址或網域，識別字除 `Exchange::Okx` 與兩個「不支援」常數外不得含 `okx`。下單類函式名稱只准在 `execution/`。
- 主機只能經 `DemoEnv`（包住 `signed::endpoints` 的 `BinanceHost` / `BybitHost`）選擇；`OrderHttpRequest` 沒有接受網址的建構子；真實傳輸層再以 `HostPolicy::SignedDemo` 擋一次。
- 為重用既有簽名與 JSON 解析，`signed::signing::Credentials` 欄位與 `signed::models` 的欄位解析函式由 `pub(super)` 放寬為 `pub(in crate::exchange)`；未修改 core 與 store。

**結果分類（D4）**
- 四分類在 `execution::classify::SubmitClass`；引擎共用契約 `SubmitOutcome` 維持三個變體：**被限流對引擎回報為 `Unknown`**（429 是否保證未處理未驗證，所以先以同一 id 查單，查到前意圖不會是 FAILED）。延遲事件的 `result` 為 `unknown`，原因字串以 `rate limited` 開頭。
- 「結果未知」：逾時、連線失敗、無法解析的 2xx、5xx、沒有交易所錯誤碼的 4xx、Binance `-1000/-1001/-1006/-1007`、Bybit `10000/10016`。「已拒絕」：帶錯誤碼的 4xx、Bybit HTTP 200 且 `retCode` 非 0。「被限流」：HTTP 429 / 418、Binance `-1003`、Bybit `10006/10018`。各錯誤碼清單**未驗證**。
- **沒送出就是確定的拒絕**：OKX、非 `demo` 前綴或格式不符的 id、未校時、限流退避中、持倉模式非單向或查不到 → `Rejected`（`code = not_sent`），意圖 FAILED 是事實（交易所上沒有這張單）。
- 限流器與唯讀簽名請求共用 `RateLimiter`（`RequestClass::Signed`，每所獨立）；退避中送單直接拒絕、查單回 `Failed`（該腿狀態不變，下一個 tick 再查）。

**請求細節（全部未驗證，4.2 實測）**
- Binance：參數放在簽名的 query string（POST / DELETE 亦同）、`type=MARKET`、`newOrderRespType=RESULT`、只有 reduce-only 單帶 `reduceOnly=true`；查單 / 撤單用 `origClientOrderId`（或 `orderId`）；手續費不在委託物件內，`query` 在有成交時加查 `userTrades` 並在單一手續費幣別時加總，否則回報「未提供」而不猜。送單 ACK 已成交但無手續費時，引擎額外查一次以寫入 `ORDER_FILL`（成交明細含手續費，使用者決定）。
- Bybit：JSON body（serde_json 排序後的鍵；簽名的字串即送出的字串）、`positionIdx: 0`、`reduceOnly` 明確給值；查單先 `realtime` 再退到 `history`；撤單成功後再查一次取得狀態；`cumExecFee` 的幣別假設為 USDT。
- 持倉模式：Binance 為帳戶層級（`positionSide/dual`），Bybit 為逐標的（`position/list` 的 `positionIdx`，沒有任何列 = 不明 → 不送單）。**只快取確認為單向的結果 60 秒**（`POSITION_MODE_TTL_MS`），不明或雙向從不快取；送單延遲因此在快取失效時包含一次模式查詢。
- `client_order_id`：只接受引擎產生的 `demo` id（`[A-Za-z0-9_-]`、≤ 36），執行器原樣傳遞不改寫。spec 例子 `demo_ab12_L_open_1` 不是引擎格式，測試改用引擎產生的 id。

**工廠、金鑰與帳戶**
- `DemoExecutorFactory` 只為 `EXCHANGE_DEMO` 建立執行器，**Binance 與 Bybit 的金鑰都必須存在且非空**（OKX 不能下單，所以每個 demo 配對都需要兩者）；讀不到、空字串或 Keychain 錯誤一律 `Err`（不送任何請求，錯誤字串只含交易所與原因）。金鑰在建立時讀一次並保存在執行器內，不每單讀 Keychain（避免反覆授權提示）；更換金鑰須切回 SIMULATION 再切回。
- `DemoAccountView` 走既有唯讀簽名 GET：持倉帶正負號（Bybit `size` + `side` 由 adapter 正規化）；**出現任何雙向持倉即回 `Err`**（引擎假設單向，加總兩邊會掩蓋曝險）；可用保證金取 USDT 的 `available`，未提供即 `Err`（失敗即封閉；Bybit UNIFIED 帳戶的 `availableToWithdraw` 可能為空，屆時 Margin 檢查會 BLOCK，**未驗證**，可能需改讀帳戶層 `totalAvailableBalance`）。
- 工廠、執行器、帳戶檢視都已實作並以錄製回應測試，但**尚未接到 `main.rs`**（本 change 不得修改；真實組裝需 `ReqwestOrderTransport`、`GatedTransport`、ClockSync 的偏移）。在接線前，4.2 以 `exchange::execution::live_probe`（`#[ignore]`，需環境變數確認）在 Mac 上實測。

**延遲事件（1.3，T−5 決策依據）**
- 每次送單寫一筆 `ORDER_LATENCY`：`request_sent_at` 取在意圖落地之後、`Executor::submit` 之前，`ack_at` 取在其回傳時（注入的時鐘）；`triggered_at` 為進場觸發（`StartCheck` 落地）或開始平倉的時間；`trigger_to_ack_ms` 一併寫入。兩腿各自 spawn，第二腿不等第一腿。
- `engine::latency::LatencyReport::from_events`：最近秩百分位（nearest rank）；`entry_to_both_accepted` = 同一次進場兩腿皆 accepted 時「較晚的 ACK − 觸發」，有一腿非 accepted 的進場排除並計數；模擬事件預設排除。`t5_criterion_met()` = p99 < 2,500 ms。

**逾時與成交確認（2.1、2.2）**
- 成交判定沿用 engine-simulation 的 `fill::fill_decision`（以 order id 查單、幣本位不平衡、等於門檻視為通過），未重寫。
- **逾時撤單改依本 spec**：engine-simulation wave 2 的「殘留委託不自動撤單」是實作時的選擇而非使用者決定，與本 spec「逾時只撤銷自己未成交的單、撤後再查最終成交量」衝突，依 spec 實作。到達生效逾時（自送單集合建立時起算，略早於最早的 `request_sent_at`，偏保守）時，對本配對尚未結束（未成交完或結果未知）的開倉單各送一次撤單再查一次，以**最終**狀態呼叫 `fill_decision`；撤後仍為未結束、或查單失敗 / 查無 → 該腿為未知 → `UNRESOLVED`。撤單結果寫 `ORDER_CANCEL_RESULT`。撤單期間舊的輪詢結果不覆蓋最終查詢。平倉單逾時不撤（已平倉確認負責）。
- `TimeoutUndetermined` 的警示原因：有一腿的送單從未得到可用回覆 → `SUBMIT_UNKNOWN`，否則 `FILL_UNCONFIRMED`。

**警示（3.1）**
- Snapshot 新增 `alerts`（由處於 `PARTIAL_FAILURE` / `IMBALANCED` / `UNRESOLVED` 的配對推導；重啟後第一份 Snapshot 即有）。每次進入警示狀態寫一筆 `PAIR_ALERT`（恰好一個原因：`SUBMIT_REJECTED`、`SUBMIT_UNKNOWN`、`FILL_TIMEOUT_ONE_LEG`、`IMBALANCE`、`RECONCILE_MISMATCH`、`CLOSE_LEG_FAILED`、`FILL_UNCONFIRMED`、`OTHER`；含兩腿的委託與成交資料）並呼叫一次 `Notifier`。
- 「同一次進入」以進入該狀態的 `PAIR_TRANSITION` 事件 id 為鍵：其後已有 `PAIR_ALERT` / `ALERT_NOTIFIED` / `ALERT_NOTIFY_FAILED` 即不再寫或通知，重啟亦同；對帳器造成的警示在對帳完成後補寫與通知。**通知失敗也算已處理**（寫 `ALERT_NOTIFY_FAILED`，不自動重試，避免洗版；橫幅與事件照常）。通知在 blocking 執行緒呼叫，不卡 actor。
- macOS 通知機制**未決定**：目前只有 `LogNotifier`（寫 stderr，不是系統通知）與測試用 `RecordingNotifier`；需在 Mac 上試 `osascript` 後再實作（TODO.md）。
- `alert::count_by_reason`：任一期間各原因次數（所有原因都列出，總和 = 警示事件數）。

**人工出口與平倉（3.2、3.3）**
- 「人工確認已平倉」**不採信 `verified_flat`**：只接受警示狀態的配對，系統重新查詢兩腿該標的的持倉與兩所的未成交委託（清單須完整），持倉皆為 0 且無委託才落地 `ConfirmClosed { verified_flat: true }` → `FINALIZED`，否則拒絕並回報兩腿持倉與委託數；Command 的回覆延到重查結束；結果寫 `MANUAL_CONFIRM_RESULT`。不屬於本配對的同標的部位也會使確認失敗（保守，與 D10 一致）。
- 已平倉確認在平倉逾時內重試（持倉更新延遲），逾時仍非 0 或仍有委託才 `PARTIAL_FAILURE`；取代原本第一次查到非 0 即失敗。
- **與 spec 衝突、依使用者決定**：pair-close spec「一腿已被強平 → 只對另一腿送單」與 2026-10-05 晚的決定「平倉量 = min(記錄量, 實際持倉)，差異超過 `max_leg_imbalance_pct` 轉人工」衝突（強平後實際 0 對記錄 10 是 100% 差異）。依使用者決定：兩腿都不送、寫 `CLOSE_QUANTITY_MISMATCH`、`PARTIAL_FAILURE`（測試 `a_liquidated_leg_is_a_quantity_mismatch_and_nothing_is_closed_per_the_user_decision`）。spec 例子「實際 0.019 對記錄 0.020」在門檻內，平 0.019 與 spec 一致。人工再平倉時，已平完的腿（記錄量 0、持倉 0）略過，只處理剩餘腿。

**驗證**
- 4.1：`cargo test -p tong-funding exchange_replay -- --nocapture`，真實工廠 / 執行器 / 帳戶檢視在錄製回應上跑完整引擎，並檢查所有請求主機在允許清單、事件不含金鑰與簽名、每次送單恰有一筆延遲事件。
- 紅燈的取得方式：先寫測試，再對「刻意退回的實作」（拿掉 id 參數、Python 式例外即失敗、不撤單、採信使用者旗標、不寫事件等）執行取得失敗輸出，然後恢復實作確認綠燈；2.1 的行為已存在於 engine-simulation，測試首次執行即綠燈。
