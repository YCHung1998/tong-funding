# 003 — Bybit demo 的資金費流水延遲超過 1 小時才出現

| | |
|---|---|
| 狀態 | **事件已記錄；原因未知；交由 Eason 接手研究**（未做任何程式修改） |
| 發現日期 | 2026-10-14（UTC） |
| 範圍 | Bybit demo（`api-demo.bybit.com`）`GET /v5/account/transaction-log`，`type = SETTLEMENT` |
| 資料來源 | `funding.db`（唯讀）：`funding_slots`、`FUNDING_LEDGER_FETCHED`、`PAIR_FUNDING_LINKED` 事件；Bybit 官方文件 |
| 相關 | [002](002-trades-2026-10-14.md)（四筆交易表）、[001](001-binance-demo-funding-rate.md) |

## 摘要

10-14 16:00 UTC 結算的三筆交易（ARC、BLUR、1000CAT），Bybit 多單的資金費流水在 16:00～17:00 的 20 次查詢都回 0 筆，直到 19:11:12 才一次查到三筆。10-15 02:00 UTC 結算的 RSR，同一個查詢方式卻在 18 秒內就查到。流水金額與用公開費率算出的值吻合，所以不是「demo 不結算」，是**出現得很晚**。

## 事件經過

| 配對 | 結算 | 流水入庫（`PAIR_FUNDING_LINKED`） | 結算後 | 金額 | 以 T-10 費率推算 |
|---|---|---|---|---|---|
| BLUR | 10-14 16:00 | 10-14 19:11:12 | 11,472 秒（約 3 時 11 分） | +4.5966 | +4.5967 |
| ARC | 10-14 16:00 | 10-14 19:11:12 | 11,472 秒 | +1.4199 | +1.3850 |
| 1000CAT | 10-14 16:00 | 10-14 19:11:12 | 11,472 秒 | +1.6824 | +1.6893 |
| RSR | 10-15 02:00 | 10-15 02:00:18 | 18 秒 | +0.8926 | +0.9407 |

16:00 那批的 Bybit 查詢紀錄（`FUNDING_LEDGER_FETCHED`，查詢窗從 15:59:00 起）：

| 時間（UTC） | 結果 | 新增筆數 |
|---|---|---|
| 16:10:45（對帳，窗 15:59:51～16:00:16） | complete | 0 |
| 16:15:02、16:20:04、16:25:06、16:30:09、16:40:00、16:50:04、17:00:07 | complete | 0 |
| 18:01:11 | **incomplete**（同時有 `FETCH_ERROR`） | 0 |
| 19:11:12 | complete | **3** |

- 追蹤器的節奏：結算後每 5 分鐘（最多到 +30 分）、每 10 分鐘（到 +60 分）、之後每小時（`MANUAL_CHECK`）。17:00:07 之後下一次成功的查詢是 19:11:12。
- 因此流水**最晚在 17:00:07 之後、19:11:12 之前**出現，實際時間點**不知道**（中間 18:01 那次失敗）。
- 同期 Binance 的流水都在結算後約 150 秒內取到，沒有延遲。

## 已確認（資料庫與文件）

- 我們的請求與官方文件一致：`accountType=UNIFIED`、`category=linear`、`currency=USDT`、`type=SETTLEMENT`；文件寫明 `SETTLEMENT` 是「USDT 永續資金費結算」，`funding` 正值＝收到；文件也列出 demo 支援 `/v5/account/transaction-log`、`/v5/execution/list`、`/v5/position/closed-pnl`；時間窗最長 7 天、每頁最多 50 筆。
- 20 次空結果都是 `complete`、沒有錯誤碼、沒有被限流（`retCode` 不是 10006），只有 18:01 那次失敗。
- 流水最終**有**出現，且金額與公開費率相符（見上表），所以 Bybit demo 有模擬資金費。

## 不知道（待研究）

1. 16:00 那批為什麼晚這麼多，而 RSR（02:00）只晚 18 秒？可能性（**都只是猜測，沒有證據**）：8 小時結算點（00/08/16）Bybit 處理較慢；ARC／BLUR／1000CAT 是三個同時結算的幣；帳戶或 demo 服務當時負載；其他。
2. 16:00 那批流水到底在幾點出現？17:00～19:11 之間只有一次失敗的查詢（18:01）。
3. 這是常態還是偶發？目前只有兩個結算點（16:00 與 02:00）的樣本。
4. 是我們查詢的問題嗎？目前沒有證據指向我們（請求符合文件、之後同樣查詢拿得到），但沒有直接排除：例如 `startTime/endTime` 是否以「入帳時間」而非「結算時間」過濾，需要用原始回應確認。

## 建議的研究方式（只讀，不下單）

1. 之後每個 Bybit 結算點，記錄「首次出現時間」：`funding_slots.first_seen_ms`、`PAIR_FUNDING_LINKED.delay_ms`（`delay_ms` 就是這個延遲，可累積成分布；目前程式已有 `delay_report`）。
2. 下一次延遲發生時，在結算後的前 3 小時內，每 5～10 分鐘手動打一次原始請求（同樣參數，另外加不帶 `type` 的版本），看 `transactionTime`、`id` 與回應何時出現。
3. 對照 Bybit 的 `execution/list`（`execType=Funding`）與 `closed-pnl` 是否比 `transaction-log` 早出現（文件沒有明說 `execution/list` 會回資金費，需要實測）。
4. 如果延遲是常態，追蹤器要把 Bybit 的等待窗拉長（目前 +60 分之後只每小時查一次），並讓 UI 顯示「流水延遲中」而非 `INCOMPLETE`（這是設計決定，等研究結果再談）。

## 對目前系統的影響

- Bybit 一腿的損益在流水出現前會停在 `INCOMPLETE`；補到之後會自動重算（`PAIR_PNL_RECOMPUTED`），所以最終數字是對的，只是來得晚。
- 先前一度被誤判為「Bybit demo 不模擬資金費」，這個判斷不成立（見 002 發現 2）；之後遇到「掃了那些時間卻沒拿到」不能直接當成 demo 沒有資金費。

## 重現（唯讀）

```bash
sqlite3 -readonly ~/Library/Application\ Support/tong-funding/funding.db \
  "select datetime(ts_ms/1000,'unixepoch'), json_extract(payload,'\$.outcome'), json_extract(payload,'\$.inserted') from events where event_type='FUNDING_LEDGER_FETCHED' and payload like '%Bybit%' order by id;"
sqlite3 -readonly ~/Library/Application\ Support/tong-funding/funding.db \
  "select datetime(ts_ms/1000,'unixepoch'), pair_id, json_extract(payload,'\$.exchange'), json_extract(payload,'\$.amount'), json_extract(payload,'\$.delay_ms')/1000 from events where event_type='PAIR_FUNDING_LINKED' order by id;"
sqlite3 -readonly ~/Library/Application\ Support/tong-funding/funding.db \
  "select pair_id, leg, exchange, state, resolution, amount, checks_ok, checks_failed from funding_slots order by settle_ms, pair_id, leg;"
```

## 相關程式位置

- 查詢：`app/src/exchange/signed/ledger.rs`（`BybitLedgerClient::transaction_log_page`）
- 抓取與分頁：`app/src/funding/fetch.rs`；追蹤節奏與延遲統計：`app/src/funding/slots.rs`、`app/src/funding/slot_runner.rs`（`delay_report`）
