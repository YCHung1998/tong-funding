## Context

**Backend 事實（以 `mvp-python` 為準）**

| 事實 | 來源 |
|---|---|
| Python 版**沒有任何** funding 流水、income、transaction-log 或 PnL 的程式碼；`binance_client.py` 與 `bybit_client.py` 的簽名 GET 只有帳戶、餘額、持倉、委託、單筆訂單；整個專案搜尋 `income`、`transaction-log`、`FUNDING_FEE`、`bills` 只命中 `/v5/account/wallet-balance` 一處不相關的路徑。因此本 change 是全新功能，所有流水解析都沒有既有程式碼可對照 | `mvp-python/binance_client.py`、`bybit_client.py`（全文閱讀）與全專案文字搜尋 |
| SYSTEM_SPEC 只有規格：§29 COMPLETED 須「Long = 0、Short = 0、Open Orders = 0、PnL Calculated」；§31 PnL = Funding + Price − Opening Fee − Closing Fee − Slippage − Other Execution Cost；§32 比較 Target / Requested / Actual Filled Notional 與 FillRatio；§33 滑價為 Expected Price、Actual Fill Price、Price Difference、Slippage %，Long = (Actual − Expected) ÷ Expected，Short 依方向 | `SYSTEM_SPEC.md` §29、§31–§33 |
| Python 版不平衡與單腿失敗事件已存在（`PAIR_PARTIAL_FAILURE`、`PAIR_FINALIZED`），但沒有 PnL 事件 | `reference/event_log_schema.md` |

**對公開文件查證的事實**（2026-10-05；只讀公開文件，**未對任何真實帳戶發請求**）

| 項目 | 文件所載 | 來源 |
|---|---|---|
| Binance 端點 | `GET /fapi/v1/income`（USER_DATA，簽名），參數 `symbol`、`incomeType`、`startTime`、`endTime`、`page`、`limit`；`incomeType` 列舉含 `FUNDING_FEE` 與 `SPECIAL_FUNDING_FEE`；回傳欄位 `symbol`、`incomeType`、`income`、`asset`、`info`、`time`、`tranId`、`tradeId` | `binance/binance-connector-js` 的 `derivatives-trading-usds-futures` 用戶端原始碼與型別（`account-api.ts`、`get-income-history-response-inner.ts`） |
| Binance 行為 | 未帶 `incomeType` 回傳所有流水；未帶時間只回最近 7 天；`tranId` 在同一 `incomeType` 內唯一；只保留最近三個月；權重 30 | 同上之註解 |
| Bybit 端點 | `GET /v5/account/transaction-log`（UTA），參數 `accountType`、`category`、`currency`、`baseCoin`、`type`、`startTime`、`endTime`、`limit`（1–50，預設 20）、`cursor`；回傳 `list[]`（`id`、`symbol`、`side`、`transactionTime`、`type`、`size`、`qty`、`currency`、`funding`、`fee`、`cashFlow`、`change`、`cashBalance`、`feeRate`、`tradeId`、`orderId`、`orderLinkId`…）與 `nextPageCursor` | `bybit-exchange/docs` 的 `v5/account/transaction-log.mdx` |
| Bybit 行為 | `funding` 正值為收到、負值為支付（與成交的 `execFee` 方向相反）；只傳 `startTime` 回該時間起 24 小時，兩者皆傳則區間不得超過 7 天；支援最多兩年資料；`type=SETTLEMENT` 為 USDT 永續的 funding 結算 | 同上與 `v5/enum.mdx`（`typeuta-translog`） |
| Bybit demo | demo 交易的支援端點表列有 `/v5/account/transaction-log`；demo 訂單只保留 7 天 | `bybit-exchange/docs` 的 `v5/demo.mdx` |

## Goals / Non-Goals

**Goals**
- 讓持倉頁看得到每腿與每組配對真正收到的 funding，並能拆解 Net PnL。
- 流水不可變、去重、可對帳，缺資料時明說而不是顯示 0。
- 讓 Net Edge 的預期可以和實際逐項比較，作為校正 `est_slippage_pct` 等欄位的依據。

**Non-Goals**
- 不取得 OKX 流水（OKX 不下單）。
- 不做 USDC 永續、幣本位合約、Binance Portfolio Margin 或 Bybit 非 UTA 帳戶。
- 不做歷史資料回補超出交易所保留範圍的部分。
- 不自動修正任何不一致的流水。
- 不做報表匯出與稅務用途的計算。
- 「其他成本」在此版沒有資料來源，只保留分量。

## Decisions

**D1　流水寫入既有 `events` 表，去重鍵以唯一索引保證，不另建流水表。**
使用者拍板「流水寫入事件表（不可變）並以交易所端 id 去重」。實作為 `store-sqlite` 之後的新 migration：在 `events` 上對 `json_extract(payload, '$.dedupe_key')` 建立僅限 `event_type = 'FUNDING_LEDGER_ENTRY'` 的部分唯一索引，寫入以 `INSERT ... ON CONFLICT DO NOTHING` 並檢查受影響列數。不更動 `events` 既有欄位，也不放寬不可變 trigger。
代價：查詢流水需 `json_extract`。**表達式索引與 `ON CONFLICT DO NOTHING` 在 bundled SQLite 的行為未驗證**，列於 task 1.4 的測試。

**D2　去重鍵：`binance:FUNDING_FEE:{tranId}` 與 `bybit:{id}`。**
Binance 文件寫明 `tranId` 在同一 `incomeType` 內唯一；Bybit `id` 文件標為 Unique id。以 `incomeType` 為鍵的一部分，避免與其他類型衝突。同鍵不同金額視為衝突事件，不覆寫（資料不可變，也避免掩蓋交易所端的修正）。

**D3　歸屬規則採「交易所＋標的＋時間窗」，不依賴流水上的訂單或持倉欄位。**
Bybit 流水有 `orderId`、`size`，但 funding 結算並非由訂單產生；Binance `FUNDING_FEE` 流水的 `tradeId` 是否為空與 `info` 內容**未驗證**。時間窗歸屬在 `ExistingExposure` 檢查保證「同交易所同標的同時只有一個配對」的前提下是無歧義的；若前提被破壞（例如人工手動下單造成同標的另一倉位），標為歸屬不明並使 PnL 為 `INCOMPLETE`，不猜測。

**D4　PnL 拆解以「參考價」計價差、以「參考價與實際成交價之差」計滑價（解釋 SYSTEM_SPEC §31）。**
§31 把價差 PnL 與滑價並列相減。若價差 PnL 以實際成交價計算，滑價已隱含其中，再扣一次就是重複計算。因此價差 PnL 以送單當下的預期價格（參考價）計，滑價 = 參考價基礎的價差 PnL − 實際成交價基礎的價差 PnL，Net PnL 同時等於「實際基礎算式」。這是我對 §31 的解讀，Python 版沒有實作可對照，列入 Open Questions。

**D5　PnL 狀態只有 `COMPLETE` 與 `INCOMPLETE`，缺資料絕不補 0。**
監控工具最大的風險是把「不知道」顯示成「沒有」。所有缺漏來源（缺結算、取得失敗、缺成交明細、缺參考價、手續費幣別、歸屬不明、對帳差異）一律落到 `INCOMPLETE` 並附原因。

**D6　送單當下必須保存預期價格與 Net Edge 快照；成交明細須含價格、數量、手續費與幣別。**
滑價、成交比、預期對實際都依賴這些資料。它們由 `engine-simulation`（送單意圖與快照）與 `exchange-demo-execution`（成交確認）寫入事件；**目前兩個 change 的 proposal 都沒有明列此需求**，需於它們的 spec 撰寫時納入（列入 Open Questions 作為跨 change 缺口）。本 change 不假設成交明細已存在，缺漏時 PnL 為 `INCOMPLETE`。

**D7　「PnL 已計算」的定義：存在該配對的 PnL 事件，狀態可為 `COMPLETE` 或 `INCOMPLETE`。**
若要求必須 `COMPLETE`，一次取不到的流水會讓已平倉的配對永遠停在 `CLOSING`，並可能持續佔用 `max_concurrent_pairs`。折衷：`INCOMPLETE` 的配對可以 `FINALIZED`，但警示持續存在，且之後資料到齊會以 `PAIR_PNL_RECOMPUTED` 補算。**這偏離 SYSTEM_SPEC §29 的字面（`PnL Calculated`）**，需使用者確認（Open Questions 第 1 項）。

**D8　對帳以「重新取得並比較」為主，容差為 0。**
兩邊皆為交易所同一端點的 Decimal 字串，應完全一致；任何差異都是資料問題而非四捨五入，因此不設容差，避免憑空編造一個容差數字。

**D9　取得時機：預期結算後延遲取得、平倉確認後再取一次、有重試窗。**
延遲秒數、重試間隔、重試窗長度**皆未驗證**（交易所發佈 funding 流水的延遲沒有查證）。暫定提議（非查證結果）：結算後 60 秒首次取得，之後每 60 秒重試，重試窗 10 分鐘。這些值放在設定常數，由 task 5.1 的真實觀察校正。

**D10　`SIMULATION` 不產生 PnL。**
沒有真實成交，也沒有流水可取；若以模擬數字填 PnL 會把「模擬」誤認為「實際」。

**D11　`pair-lifecycle` 的修改以 delta spec 的 `## MODIFIED Requirements` 寫在本 change。**
見下節「依賴與封存順序」。

## 依賴與封存順序

- `pair-lifecycle` 目前只存在於尚未封存的 change `core-domain-and-fixtures`，主 spec 目錄 `openspec/specs/` 尚無該 capability。
- 本 change 的 delta 使用 `## MODIFIED Requirements`，且 requirement 標題「FINALIZED 須確認已平倉」與 `core-domain-and-fixtures` 內的標題一字不差。`openspec validate funding-pnl` 已通過（2026-10-05 實測），代表 validate 不檢查主 spec 是否存在。
- **封存順序必須是：先封存 `core-domain-and-fixtures`，再封存 `funding-pnl`。** 反之主 spec 尚不存在，MODIFIED 無對象可修改。**封存時的實際行為未驗證**（尚未執行 `openspec archive`）。
- 若封存時 `openspec` 因主 spec 不存在而失敗，替代方案是把本 delta 改為 `## ADDED Requirements` 並刪除與舊版重複的部分；但這會與 core 的同名 requirement 衝突，因此較安全的做法是維持封存順序。
- 若 `core-domain-and-fixtures` 在封存前修改了該 requirement 的標題，本 change 的 MODIFIED 標題須同步更新。
- 實作順序同樣：`core` 的 `pair-lifecycle` 先完成，本 change 的 task 3.1 再擴充 `FINALIZED` 的確認。

## 驗證紀錄（task 1.1、5.1 執行後填寫）

| 項目 | 文件所載 | 對真實 demo 驗證結果 |
|---|---|---|
| Binance demo / testnet 主機是否支援 `/fapi/v1/income` | 文件有此端點；demo 主機是否支援**未驗證** | 待驗證 |
| Binance `income` 金額正負號（收到為正？） | 文件未明述，視為**未驗證** | 待驗證 |
| Binance `FUNDING_FEE` 的 `asset`、`info`、`tradeId` 實際內容 | **未驗證** | 待驗證 |
| Binance 單頁筆數上限與 `page` 分頁行為 | `page`、`limit` 參數存在；上限數字**未驗證** | 待驗證 |
| Binance 在 demo 帳戶是否真的產生 funding 流水、頻率 | **未驗證** | 待驗證 |
| Bybit demo 是否實際回傳 `SETTLEMENT` 流水 | demo 端點表有列此端點；實際內容**未驗證** | 待驗證 |
| Bybit `funding` 正負號 | 文件：正值為收到 | 待驗證 |
| Bybit `limit` 上限 50、7 天窗、`nextPageCursor` | 文件所載 | 待驗證 |
| 兩所結算後流水出現的延遲 | **未驗證** | 待驗證 |
| funding rate 為 0 的結算是否仍有流水 | **未驗證** | 待驗證 |
| 成交手續費的幣別（Binance 可能為 BNB 抵扣？） | **未驗證** | 待驗證 |

實際走完一組配對的數字（task 5.1）：待填。

## Risks / Trade-offs

- **demo / testnet 可能不產生真實的 funding 流水，或格式與正式環境不同。** 這會使整個 change 在 demo 下無法驗證。緩解：task 1.1 先驗證；若 demo 沒有流水，需在 Open Questions 與使用者討論（例如僅以單元測試與錄製 fixtures 驗證）。
- **對帳的獨立性有限。** 重新取得與原始取得來自同一端點，只能偵測取得過程的遺漏或交易所事後修正，無法偵測交易所端資料本身有誤。另一個獨立來源（例如帳戶餘額變動、Bybit 成交紀錄的 funding 類型）**未查證可行性**，不納入此版。
- **預期對實際的基準假設可能不成立。** `net-edge` 假設單次結算與四筆成交；人工處理後長時間持倉會跨過多次結算，比較結果已要求標示此差異，但差異金額仍不具校正意義。
- **參考價的選擇會左右滑價數字。** 以送單當下記錄的預期價格為準（D6）；若該價格取自過期快取，滑價會被高估或低估。依賴 `pretrade-validation` 的「最新資料」要求。
- **PnL 為 `INCOMPLETE` 仍可 `FINALIZED`（D7）可能讓使用者忽略不完整的結果。** 緩解：警示持續、持倉頁與面板頂部明示。
- **重試窗與延遲值為暫定。** 若交易所發佈流水比預期慢，會產生假的「缺少」警示。
- **Binance 流水只保留三個月。** 超過範圍的歷史永遠無法回補；事件表本身永久保存，所以越早取得越好。

## Open Questions

1. **（需人類決定，偏離 SYSTEM_SPEC §29 字面）** `INCOMPLETE` 的 PnL 是否算「PnL 已計算」而允許 `FINALIZED`（D7）？若否，需決定重試窗結束後仍缺資料的配對該停在哪個狀態、是否佔用 `max_concurrent_pairs`。
2. **（跨 change 缺口，需人類決定）** `engine-simulation` 與 `exchange-demo-execution` 目前的 proposal 都沒有要求保存「預期價格、Net Edge 快照、成交明細的價格／數量／手續費／幣別」（D6）。是否同意在那兩個 change 的 spec 納入？
3. **（解釋性決定）** D4 對 SYSTEM_SPEC §31 的解讀（價差 PnL 用參考價、滑價單獨扣）是否符合使用者意圖？
4. funding rate 為 0 的結算是否仍會產生流水？若不會，「缺少結算流水」的判定會誤報；目前保守地視為缺少。
5. Binance 手續費若以 BNB 抵扣，是否要支援換算？目前判為 `INCOMPLETE`。
6. 對帳是否需要第二個獨立來源（見 Risks）？
7. 重試窗與延遲的暫定值（D9）是否接受，或等 task 5.1 實測後再定？
8. 新增的事件類型（`FUNDING_LEDGER_ENTRY`、`FUNDING_LEDGER_CONFLICT`、`PNL_RECONCILIATION`、`PAIR_PNL_COMPUTED`、`PAIR_PNL_RECOMPUTED`）需併入事件結構文件與系統日誌頁的類型清單（`ui-readonly-pages`）。

## 未驗證清單

- Binance demo / testnet 是否支援 `/fapi/v1/income`、是否產生 funding 流水、`income` 正負號、`asset` / `info` / `tradeId` 內容、單頁上限與分頁。
- Bybit demo 是否實際回傳 `SETTLEMENT` 流水及其延遲。
- 兩所發佈流水的延遲；funding rate 為 0 時是否仍有流水。
- 手續費幣別（是否可能非 USDT）。
- SQLite 表達式部分唯一索引與 `ON CONFLICT DO NOTHING` 的行為。
- `openspec archive` 對 MODIFIED 的實際行為（封存順序節）。
- D9 的延遲、重試間隔、重試窗（暫定值，非查證結果）。

## 決定紀錄（2026-10-05 晚，使用者）

- **重試窗結束仍缺資料：允許 `FINALIZED` 並標記 `INCOMPLETE`**（Open Question 1），列出缺少的項目，之後資料到齊可重算；釋出 `max_concurrent_pairs` 名額。
- **同意在 `engine-simulation` 與 `exchange-demo-execution` 的 spec 納入預期價格、Net Edge 快照與成交明細**（Open Question 2）。
