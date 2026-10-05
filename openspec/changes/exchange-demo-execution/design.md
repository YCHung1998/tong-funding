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
