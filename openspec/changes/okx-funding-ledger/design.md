## Context

### 現況（程式證據）

- `signed/ledger.rs:1-38`：只有 `BinanceLedgerClient`（`/fapi/v1/income`，每標的）與 `BybitLedgerClient`（`/v5/account/transaction-log`，全帳戶、游標分頁）；註解「OKX has no ledger client (no orders there)」。
- `funding/fetch.rs:56-66`：`LedgerSource { exchange, per_symbol, page(symbol, start, end, token) }`；`MAX_WINDOW_MS` 7 天、`MAX_PAGES` 20、限流重試 3 次。
- `funding/fetch.rs:234-236`：規劃抓取時跳過 OKX 視窗。
- `funding/runner.rs:202-205`：OKX 腿沒有來源，`reconcile_pair` 把它記為 FAILED。
- `funding/pnl_record.rs:14-16`：「OKX legs: quantities are contracts and no contract value is recorded with the fill … prices treated as unknown and funding as not fetched」；`:150` 數量直接取 `filled_quantity`；`:160-167` OKX 的預期價與成交價為 `None`；`:316-319` OKX 的 funding 固定 `NotFetched`。
- `core/src/pnl.rs:70`：流水去重鍵已有 `okx:{kind}:{exchange_id}`；`:101-113` `FillRecord.quantity` 為幣量。
- `engine/actor.rs:2365-2376`：成交事件的 `filled_quantity` 為交易所下單單位（OKX 張數），未記錄 `ct_val`；`actor.rs:1075` 與 `recovery.rs:720-723` 在需要時從 `order_rules` 取 `ct_val`。

### OKX API v5 事實（https://www.okx.com/docs-v5/en/ ，2026-10-07 取得）

- **Account → Get bills details (last 7 days)**：`GET /api/v5/account/bills`，5 次 / 秒。
- **Account → Get bills details (last 3 months)**：`GET /api/v5/account/bills-archive`，5 次 / 2 秒，User ID。
- 共同參數：`instType`、`instId`、`ccy`、`mgnMode`、`ctType`、`type`、`subType`、`after`（回傳比該 billId 更早的資料）、`before`、`begin` / `end`（`ts` 毫秒）、`limit`（最大 100）；結果依新到舊排序。
- 回應欄位：`billId`、`type`、`subType`、`ts`、`balChg`（「Signed change in account balance … Positive: balance increased」）、`ccy`、`instId`、`pnl`、`fee`。
- `subType` `173` = Funding fee expense、`174` = Funding fee income；完整對照可由 `GET /api/v5/account/subtypes` 取得（type `8` 為資金費，本計畫以實機 `subtypes` 回應確認）。

## Goals / Non-Goals

**Goals:**
- OKX 腿的 funding 能被取得、去重、歸屬到配對，且「抓不完整」與「確實沒有」可區分（沿用 `funding-history-fetch` 的 fail-closed 規則）。
- OKX 腿的成交以正確的幣量與價格進入 PnL；舊資料不被誤算。
- 對帳涵蓋 OKX 腿。

**Non-Goals:**
- 2021 年以來的歷史帳單（`bills-history-archive` 需申請檔案）；超過 3 個月的視窗以失敗呈現。
- 交易手續費的帳單對帳（手續費取自訂單查詢，與另外兩所一致）。
- 回補本 change 之前、沒有 `ct_val` 的 OKX 成交事件。

## Decisions

**D1　一律使用 `bills-archive`（3 個月）並以 `begin`/`end` 限定 7 天視窗。**
一條程式路徑涵蓋全部保留期；`funding/fetch.rs` 的抓取時機（結算後 60 秒起、重試 10 分鐘）每分鐘至多數個請求，遠低於 5 次 / 2 秒。
- 替代：視窗在 7 天內用 `bills`、否則用 `bills-archive`。多一條路徑；只有在實機發現 `bills-archive` 對最新帳單有延遲時才改（Open Question 1）。

**D2　每標的查詢（`instId=<BASE>-USDT-SWAP`），`per_symbol = true`。**
與 Binance 相同，`merge_plans` 依標的合併；避免全帳戶查詢的分頁量。
- 替代：全帳戶查詢（Bybit 做法）。分頁更多、且仍需在本地過濾標的。

**D3　分頁：以本頁最後一筆的 `billId` 作 `after`，直到該頁少於 `limit`；遇到重複 `billId` 或達到頁數上限 → 無法確認完整（失敗）。**
`LedgerPage.next_cursor` 存下一個 `after` 值；`rows` 為該頁原始筆數（含被過濾掉的非資金費列）。

**D4　金額取 `balChg`，以 `subType` 驗證正負號；`ccy` 必須是 `USDT`。**
173 的 `balChg` 必須 ≤ 0、174 必須 ≥ 0；不一致或 `ccy` 不是 USDT → 解析錯誤（整頁失敗，不寫入），避免靜默記錯方向。
- 替代：取 `pnl` 欄位。文件未說明資金費列 `pnl` 的語意；`balChg` 有明確的正負號定義。

**D5　`ct_val` 寫入成交事件，PnL 只用事件內的值。**
engine 送 OKX 單時已持有 `ct_val`（`fill.rs` 的 `LegSizing.okx_ct_val`），在 `ORDER_SUBMITTED` / `ORDER_FILL` 的 payload 加 `"ct_val"`。`pnl_record` 以 `filled_quantity × ct_val` 為幣量；缺 `ct_val` 時該筆成交數量與價格視為未知（PnL INCOMPLETE，原因「OKX 成交缺合約面值」）。
- 替代：PnL 計算時向公開 instruments 查 `ctVal`。面值可能被交易所調整，事後查詢的值不一定是成交當下的值，且讓 PnL 依賴網路。

## Risks / Trade-offs

- [demo 環境是否產生資金費帳單、`subType` 是否為 173/174] → 實機 task 以一個跨越結算的 OKX demo 配對確認；若 demo 不產生帳單，OKX 腿 funding 將以「已抓取、0 筆」呈現，需由使用者判斷是否接受（Open Question 2）。
- [`bills-archive` 對剛結算的帳單可能有延遲] → 沿用既有的重試視窗；實機記錄結算到帳單出現的延遲。
- [舊 OKX 成交事件沒有 `ct_val`] → 本 change 之前不可能有 OKX demo 成交（執行器一律 not_sent），實務上不存在；仍以 INCOMPLETE 處理。
- [修正 `FillRecord.quantity` 單位可能改變既有 PnL] → 只影響 OKX 腿，而既有 OKX 腿在本 change 前不可能有成交；以測試鎖定 Binance / Bybit 的 PnL 不變。

### OKX 特有陷阱

- 帳單分頁游標是 `billId`，方向是「更早」（`after`），與 Bybit 游標、Binance 時間分頁都不同。
- `instId` 是 `BTC-USDT-SWAP`，入庫時要轉回 `BTCUSDT`，否則無法歸屬到配對。
- 成交量是張數；PnL、滑價、成交比全部要先乘 `ct_val`。
- 資金費帳單的 `type` / `subType` 是數字字串。

## Migration Plan

- 事件 payload 新增 `ct_val`（附加欄位，讀取端對缺欄位容忍）；無資料表變更。
- 回復：移除 OKX 來源即回到「OKX 腿未取得」。

## Open Questions

1. `bills-archive` 是否即時包含最新帳單（或需改用 `bills`）？
2. OKX demo 是否真的產生資金費帳單？
3. 是否需要啟動時以 `GET /api/v5/account/subtypes` 自我檢查 `type=8` 與 173/174 的對應？（目前只在實機驗證一次。）

## 實作時發現

- 「缺 `ct_val` → INCOMPLETE 並註明原因」需要 `core` 能表達該原因：新增 `FillRecord.contract_value_missing`（`#[serde(default)]`）與 `IncompleteReason::MissingContractValue`（標籤「OKX 成交缺合約面值」）；`leg_pnl` 對該成交不計價格分量（避免把張數當幣量），手續費照常。
- `ct_val` 由 engine 的 `okx_ct_val_of(pair, leg)` 取自該腿**開倉單**的 `unit_base`（平倉單的 `unit_base` 是 1，不是合約面值）。重啟後 flow 不在記憶體（orphan / recovery 路徑）的 OKX 成交事件不帶 `ct_val`，PnL 因此為 INCOMPLETE 並指名原因——這是刻意的「不猜」，不是缺陷；若要回補需另案。
- `FundingFetchState` 對 OKX 沿用 `fetch_state`（依 `FUNDING_LEDGER_FETCHED` 事件判定），不再固定 `NotFetched`。
- `ui/live.rs` 的 OKX 流水來源接線屬 `okx-trading-enablement` 3.5：它同時建立 `OkxSignedClient`、校時與 `with_okx`，把兩處放在一起才不會各自建一份客戶端。
- 流水客戶端 `OkxLedgerClient` 包 `OkxSignedClient`（`get_signed`）：簽名、模擬標頭、`50102` 重試、閂鎖與限流判讀與唯讀客戶端完全相同，沒有第三份 attempt 迴圈。
