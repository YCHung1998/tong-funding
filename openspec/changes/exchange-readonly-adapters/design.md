## Context

來源：`mvp-python`（HANDOFF.md 的 Safety invariants 與 Fragilities、`binance_client.py`、`bybit_client.py`、`okx_client.py`、`binance_ws_feed.py`、`instrument_status.py`、`scanner.py`、`app.py`）與 2026-10-05 對公開、不需簽名端點的唯讀查證（`curl` 與一個 25 秒的 WebSocket 取樣）。
本 change 的範圍是 `app` 內的 `exchange` 模組。型別來自 `core`（`FundingObservation`、`Quantity`），金鑰與事件來自 `store-sqlite`。**沒有送出任何簽名請求，也沒有讀取 `.env`。**

### 已驗證的事實（2026-10-05，公開端點）

| 項目 | 觀察結果 | 來源 |
|---|---|---|
| Binance 週期涵蓋 | `fundingInfo` 801 筆；`exchangeInfo` 中 `TRADING` + `PERPETUAL` + `USDT` 共 525 檔，**全數**在 `fundingInfo` 內（缺漏 0）。週期分布：4h 467、8h 333、1h 1。（拍板時的數字是 528，標的數會隨上下架變動） | `GET /fapi/v1/fundingInfo`、`/fapi/v1/exchangeInfo` |
| Binance 不可交易標的 | `premiumIndex` 回 923 檔，`exchangeInfo` 中 `TRADING` 的全部合約為 785 檔 | `GET /fapi/v1/premiumIndex` |
| Binance 限流上限 | `exchangeInfo.rateLimits`：`REQUEST_WEIGHT` 2400／分鐘；`ORDERS` 1200／分鐘與 300／10 秒。回應標頭有 `x-mbx-used-weight-1m` | `exchangeInfo` 與回應標頭 |
| Binance WebSocket 更新頻率 | `!markPrice@arr`（預設）每標的間隔 2997–3003 ms；`!markPrice@arr@1s` 間隔 997–1003 ms。取樣 25 秒，958 檔。單則訊息可只含部分標的（745 與 213 檔各出現過） | WebSocket 取樣 |
| Bybit 分頁 | `instruments-info` 不帶 `limit` 只回 500 筆且 `nextPageCursor` 非空；`limit=1000` 一次回 890 筆且 cursor 為空。`tickers` 回 896 筆、無 cursor，頂層有 `time` | `GET /v5/market/instruments-info`、`/tickers` |
| Bybit 週期 | `fundingInterval`（分鐘）：480×430、240×418、60×2、**0×40**（這 40 檔是 `LinearFutures` 到期型合約）。`tickers` 另有 `fundingIntervalHour`，與 `instruments-info` 在 850 檔永續合約上全數一致 | 同上 |
| OKX 欄位語意 | `funding-rate?instId=ANY` 回 729 筆（SWAP 500、FUTURES 229），USDT 永續 485 筆；週期 8h×271、4h×214。`fundingTime` 是**即將**結算的時間（`prevFundingTime` + 週期 = `fundingTime`），`nextFundingTime` 是再下一次；`nextFundingRate` 為空；`method` 全為 `current_period` | `GET /api/v5/public/funding-rate` |
| OKX 成交量單位 | BTC-USDT-SWAP：`volCcy24h = 58273.3154`（BTC 數量，× `last` 86184 約 50.2 億 USDT）；Binance 同日 BTC `quoteVolume` 約 101.8 億 USDT。故 `volCcy24h` 是幣量不是 USDT | `GET /api/v5/market/tickers` |
| OKX 合約規格 | `instruments`（SWAP）500 筆、`live` + `linear` + `-USDT-SWAP` 485 筆；BTC-USDT-SWAP：`ctVal 0.01`、`lotSz 0.01`、`minSz 0.01` | `GET /api/v5/public/instruments` |
| 週期與下次結算的一致性 | Binance 525 檔、Bybit 850 檔永續合約，皆滿足「下次結算時間 − 資料時間 ≤ 週期」，違反 0 檔 | 自上列資料計算 |
| 延遲（本機網路，每項 3 次） | 單一標的：Binance 0.22–0.26 s、Bybit 0.16–0.18 s、OKX 0.19–0.22 s；批次：Binance `premiumIndex` 0.68–1.47 s、Bybit `tickers` 0.49–2.54 s、OKX `funding-rate?instId=ANY` 0.32–0.36 s | `curl -w %{time_total}` |
| 校時端點 | `GET /fapi/v1/time`、`/v5/market/time`、`/api/v5/public/time` 皆可用 | `curl` |
| demo 主機可達 | `testnet.binancefuture.com` 與 `demo-fapi.binance.com` 的 `/fapi/v1/time` 都回 200；`api-demo.bybit.com` 的 `/v5/market/time` 回 200。**哪個 Binance 主機接受使用者的簽名請求尚未驗證** | `curl` |

### Python 版對照（`mvp-python`，事實以原始碼為準）

- `binance_client.py`、`bybit_client.py`、`okx_client.py` 的公開行情函式一律打**正式環境主機**（`fapi.binance.com`、`api.bybit.com`、`www.okx.com`），註解說明理由是「funding rate 是市場事實、不是帳戶狀態」；只有簽名請求使用 `.env` 的 demo/testnet `BASE_URL`。
- OKX 的 demo 與正式環境是**同一個主機**，只靠簽名請求的 `x-simulated-trading: 1` 標頭區分（`reference/okx_integration.md`）。
- Python 版 `app.py` 取 OKX 下次結算時間時是用 `funding-rate.fundingTime`（正確），並把 `volCcy24h` 直接塞進名為 `quoteVolume24h` 的欄位（**單位錯誤**，見上表）。

## Goals / Non-Goals

**Goals**
- 讓後面的頁面與引擎第一次能用真實的唯讀資料運作，並修掉 Python 版已確認的缺陷（見下方對照表）。
- 讓「資料是否新鮮」成為每個讀取介面都必須回答的問題，而不是呼叫端要記得去檢查的事。
- 讓「簽名請求只能抵達 demo/testnet」在結構上成立，而不是靠設定值正確。

**Non-Goals**
- 不做任何會改變交易所狀態的請求（下單、撤單、改槓桿）；那是 `exchange-demo-execution`。
- 不做 OKX 簽名端點（Python 版從未在真實 demo 帳戶驗證成功，HANDOFF Fragility #6）。
- 不做訂單簿深度、funding 收付流水（`funding-pnl`）、資產歷史記錄。
- 不做 UI；時鐘、狀態與健康資料以型別提供，畫面在 `ui-readonly-pages`。

## Decisions

**D1　公開客戶端與簽名客戶端是兩個型別，主機名稱依型別分開。**
公開客戶端連正式環境主機（只能 GET 公開端點、沒有金鑰欄位）；簽名客戶端的主機是編譯期常數，只含 demo/testnet。
理由：`.env.example` 的 `BINANCE_BASE_URL` 可被改成任何值，Python 版的「不做真錢」是靠使用者自律。新設計讓正式環境主機只出現在無法簽名的程式碼裡，靜態掃描測試可以檢查這件事。
**與已拍板決策「端點寫死 demo/testnet」的字面差異**：公開行情打的是正式環境主機（未簽名、不涉及資金與金鑰）。見 Open Questions #1。

**D2　時間一律由注入的 `Clock` 提供。**
`core` 不讀時鐘；`app` 的 adapter 也不直接呼叫系統時間，而是接 `Clock` trait（正式版為系統時間，測試版為可手動撥動）。
理由：校時偏移量、退避、新鮮度判定都是「時間 × 狀態」的邏輯，測試必須能確定性地重現（Python 版 `trade_pipeline` 與背景執行緒的時間依賴難以測）。

**D3　Binance WebSocket 採 `!markPrice@arr@1s`，而不是 Python 版的預設 3 秒版本。**
實測預設版每標的 3 秒才更新一次，會讓 1 秒的新鮮度門檻幾乎永遠不成立；1 秒版間隔約 1 秒。代價：訊息量約三倍（取樣 25 秒內 51 則對 17 則）。
這不改變 `stale_data_threshold_ms = 1000` 的拍板值，只是讓來源本身有機會達到那個量級；仍不足以讓「快取」滿足 1 秒（見 D4）。

**D4　送單前一律重新抓取；來源層級的過期門檻是 `max(stale_data_threshold_ms, 3 × 預期更新週期)`。**
實測 1 秒版 WebSocket 的每標的更新間隔約 1000 ms，加上網路抖動，快取的資料年齡會間歇性超過 1000 ms；Bybit、OKX 輪詢（Python 版 10 秒）更是如此。若直接用 1000 ms 判斷「來源是否過期」，橫幅會持續閃爍而失去意義。
因此分兩層：
1. **來源層級**（給橫幅與資料新鮮度指示用）：門檻取 `max(stale_data_threshold_ms, 3 × 預期更新週期)`。「3 倍」是我的提議，**未驗證**。
2. **送單層級**（給 `DataFresh` 用）：不使用快取，單一標的重新抓取後 `observed_at` 就是收到時間，年齡只剩請求延遲。實測單一標的延遲 0.16–0.26 秒，低於 1000 ms，但這是單一機器、單一時段的觀察，**不保證**；超過時 `DataFresh` 失敗是設計預期（fail-closed）。
批次端點不用於送單前檢查（實測 0.5–2.5 秒，必然超過 1000 ms）。

**D5　週期資料與行情資料分開快取，並以一致性檢查抓出過舊的週期。**
週期（`fundingInfo`、`instruments-info`）很少變動，行情每秒變動，不應綁在一起輪詢。快取週期的存活時間暫定 1 小時（沿用 Python 版 `instrument_meta` 的 TTL，**適當性未驗證**）。
交易所可調整週期，所以每筆觀測做一致性檢查（`next_funding_time − 參考時間 ≤ 週期 + 容差`），違反即 `DATA_ERROR` 並觸發重抓。容差暫定 60 秒（**未驗證**；實測 1375 檔皆在容差 0 內成立）。

**D6　不完整的資料目錄不得被解讀為「不存在」。**
Python 版 `bybit_client.get_instruments_info` 的 docstring 記載：未分頁時只取得 500/891 筆，曾使真實、可交易的標的（例如 XRPUSDT）被判為未上市。新設計把「分頁取完」寫成規則，並明確區分 `Incomplete`（資料缺）與 `NOT_LISTED`（確定不存在）；缺資料的標的是 `DATA_ERROR`。分頁頁數上限暫定 20 頁（**未驗證**，實測 Bybit 一頁即完整）。

**D7　OKX 成交量以 `volCcy24h × last` 換算。**
OKX tickers 沒有 USDT 成交額欄位。以最新成交價換算是近似（用 24 小時內的平均價會更精確，但 tickers 沒有）。偏差方向與大小未量化，**未驗證**；`min_24h_volume_usdt` 是門檻型判斷，近似誤差通常不改變結論，但接近門檻時可能。

**D8　OKX 只做公開行情，帳戶方法回傳「不支援」。**
理由與 Non-Goals 相同。回傳「不支援」而非空列表，是為了讓總覽頁能區分「沒有資產」與「無法取得」。

**D9　HTTP 與 WebSocket 函式庫的選擇暫定 `reqwest`（rustls）+ `tokio` + `tokio-tungstenite`。**
這是我的提議，**尚未在這台機器上編譯驗證**，也未評估與 `gpui` 的 async runtime 共存方式（GPUI 有自己的 executor）。`bootstrap-gpui-shell` 的 task 1.4 曾用 `cargo tree` 證明 `app` 沒有這些依賴，本 change 會有意改變這件事，須在 task 1.1 重新記錄。

**D10　錯誤型別封閉，網路例外在 adapter 邊界轉換。**
Python 版 `trade_pipeline` 只接 `*ApiError`，不接 `requests` 的連線與逾時例外，會使流程中途崩潰。新設計讓 adapter 的每個方法只回傳 `AdapterError`；逾時預設值為單一標的 2 秒、批次 10 秒、簽名請求 5 秒（我的提議，**未驗證**；Python 版為 10–15 秒）。

## 與 Python 版缺陷的對照

| 項目 | Python 版 | 本 change | 驗證方式 |
|---|---|---|---|
| 取 WS 快取價不檢查連線狀態與最後訊息時間（`app.py:_get_latest_price`） | 回傳裸價格 | 快取讀取介面強制附帶來源狀態與年齡；送單前不使用快取 | feed-health 的兩個 scenario |
| 逾時 10 秒且只接 `*ApiError` | requests 例外未被接住 | `AdapterError` 封閉、逾時分級 | 逾時測試 |
| 批次端點未檢查分頁 cursor | `get_instruments_info` 已修，持倉與委託列表仍未處理 | 所有分頁端點取完或標 `Incomplete` | 分頁截斷測試 |
| 本機時鐘未與交易所校時 | 簽名用本機時間 | 校時偏移量、未校時不簽名 | 偏移量純函式測試 |
| 429 無退避 | 無 | `Retry-After` 與指數退避 | 退避測試 |
| 簽名請求的主機可由 `.env` 改成任意值 | `BINANCE_BASE_URL` | 編譯期常數 | 靜態掃描測試 |
| OKX 成交量單位錯誤 | `volCcy24h` 當 USDT | 乘以 `last` | 單位換算測試 |
| 重複的失敗事件（`FETCH_ERROR` 在舊事件檔有 1,168 筆） | 每次輪詢都記 | 只記狀態轉換 | 連續失敗測試 |
| `Binance 餘額加總把名目本金算進總資產`（`portfolio_value.py`） | 餘額 + 持倉名目 | 本 change 只提供原始餘額與持倉；總資產定義在 `ui-readonly-pages` | 見該 change |

## Risks / Trade-offs

- **1000 ms 對任何非「剛抓取」的資料都偏緊。** 這是已拍板的取捨；本 change 的緩解是 D3、D4（重新抓取）。若重新抓取的延遲在使用者的網路下經常超過 1000 ms，送單前檢查會頻繁失敗，屆時需要使用者重新決定門檻，而不是在 adapter 內放寬。
- **Binance demo 的簽名主機未確定。** 兩個主機的公開端點都可達，但簽名是否被接受只能用使用者的金鑰驗證。task 4.2 的結果決定常數值。
- **demo 主機的公開行情與正式環境不一定一致。** Python 版選擇打正式環境主機正是因此。若使用者要求公開行情也只打 demo 主機，掃幣頁的資料會偏離真實市場。
- **Bybit 與 OKX 的限流代碼與數值未驗證。** 本 change 只實作通用的 429 與 `Retry-After` 處理，以及 Binance 權重的 80% 暫緩；Bybit、OKX 的限流行為需在 task 4.1 的故障注入測試加上錄製回應，並在 task 4.2 以真實請求觀察。
- **WebSocket 訊息量上升。** 1 秒版約每秒一則含數百筆標的的訊息（單則最大約 125 KB，取樣值），解析成本需在 `ui-readonly-pages` 的 528 列效能量測中一併觀察。
- **GPUI executor 與 tokio 的共存方式未定。** 若不相容，退路是把 I/O 放在獨立執行緒的 tokio runtime，以 channel 與 UI 溝通；這會影響 `engine-simulation` 的 actor 設計。

## Open Questions

1. **公開行情打正式環境主機是否可接受？**（需要使用者決定）拍板文字是「端點寫死 demo/testnet（真錢在結構上做不到）」。本設計把「真錢做不到」落在**簽名**這一層：正式環境主機只能由無法簽名的公開客戶端抵達。若使用者要求公開行情也只用 demo 主機，需接受資料可能與真實市場不同，並重新驗證三所 demo 公開端點是否提供 funding 週期欄位。
2. **Binance 簽名請求該用 `testnet.binancefuture.com` 還是 `demo-fapi.binance.com`？** 兩者的公開端點都可達；`.env.example` 以前者為預設並註明後者為備案。需 task 4.2 以真實 demo 金鑰驗證。
3. **Binance 非 USDT 資產與合約權益如何取得 USDT 估值？** Python 版直接加總 `/fapi/v2/balance` 的 `balance`、沒有換算。`/fapi/v2/balance` 與 Bybit `wallet-balance` 的實際欄位（尤其是多資產模式）只能用真實 demo 帳戶確認，目前全部**未驗證**。
4. **Bybit 持倉與委託列表的預設頁大小與 `limit` 上限。** 依記憶（Bybit 文件）預設 20、上限 200，**未驗證**；需在 task 3.2 以真實帳戶或文件確認。
5. **帳戶持倉模式（one-way 或 hedge）。** Python 版假設 one-way；若帳戶是 hedge mode，同一標的會有兩列同方向相反的持倉，配對與平倉邏輯會受影響。需查詢並顯示持倉模式，欄位**未驗證**。
6. **週期快取 1 小時、容差 60 秒、頁數上限 20、逾時值、退避參數、「3 倍」係數**均為我的提議，未驗證，請於 task 4 的實測後確認。
7. **是否要把 Bybit `tickers.fundingIntervalHour` 作為主要週期來源？** 它與 `instruments-info` 在 850 檔上完全一致，且與行情同一個請求，不需另外快取。目前以 `instruments-info` 為主（拍板決策），`fundingIntervalHour` 只用於交叉檢查。該欄位是否為文件承諾的穩定欄位**未驗證**。

## 未驗證項目清單

Bybit 與 OKX 的限流數值與限流代碼；Bybit 持倉與委託的分頁參數；Binance 與 Bybit 簽名端點在 demo 環境的實際欄位；簽名請求的 demo 主機；多資產與 hedge mode 行為；`reqwest` / `tokio-tungstenite` 與 GPUI 的相容性；OKX 成交量換算的偏差；週期快取 TTL、一致性容差、頁數上限、逾時值、退避參數、3 倍係數；單一標的延遲在其他網路環境下是否仍低於 1000 ms。

## 決定紀錄（2026-10-05，使用者）

- **新鮮度**：`stale_data_threshold_ms` 維持 1000；送單前檢查一律單標的重抓；來源層級（橫幅與資料新鮮度指示）的過期門檻採 `max(1000, 3 × 更新週期)`。
- **端點**：公開行情可以打正式主機（唯讀、無金鑰）；真錢隔離靠簽名行為——簽名客戶端的主機為編譯期常數、只含 demo/testnet（或交易所提供的模擬倉）。
- OKX 成交量以 `volCcy24h × last` 換算為 USDT、排除 Bybit 到期型合約（`fundingInterval = 0`）：使用者無異議。
