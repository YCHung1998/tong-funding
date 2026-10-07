## Context

### 現況（程式證據）

| 頁面 / 模組 | OKX 現況 | 證據 |
|---|---|---|
| 可下單集合 | 寫死 Binance、Bybit | `ui/vm/bridge.rs:29-36` |
| 掃幣：達標與方向 | 只在可下單集合內計算；OKX 儲存格 `compare_only` | `ui/vm/scanner.rs:3,33-34,59-64,409,416,497,513` |
| 候選勾選 | `CandidateBlock::CompareOnly`（「OKX 僅比價」） | `ui/vm/candidates.rs:19-20,36,70-71` |
| 手動下單 | 只有兩個面板；選 OKX 即「OKX 不提供下單」；數量以 `Quantity::round_down(raw, lot)` 取整並標示幣名（OKX 的 lot 是張數，直接套用會把張數當幣量顯示） | `ui/vm/manual_order.rs:57,95-114,151,165-166,298` |
| 交易單頁保證金 | `ACCOUNT_EXCHANGES` 兩所 | `ui/vm/staged_orders.rs:18,347` |
| 總覽 | 固定 `ExchangeCard::CompareOnly`（「僅比價，不提供帳戶資料」） | `ui/vm/dashboard.rs:17,89-90,242-245`、`ui/pages.rs:230` |
| 持倉頁 | 固定註記「OKX 僅比價，不顯示持倉」 | `ui/vm/positions.rs:21,255,343` |
| 合約設定試算 | 已支援 OKX 張數（`Quantity::okx_contracts`、`MatchedLeg.ct_val`） | `ui/vm/contract_settings.rs:147,203-205,240-242,261` |
| 風控設定 | OKX taker fee 已必填；配對預覽含 OKX | `ui/vm/risk_settings.rs:108,485` |
| engine | `allowed_exchanges` 預設三所；張數與 `ctVal` 已處理 | `core/src/risk.rs:119`、`engine/fill.rs:49-55` |

當初排除 OKX 的設計（`ui-readonly-pages` D4）理由是「OKX 不能下單，若讓它參與達標，使用者會看到永遠無法執行的達標」——前兩個 change 完成後此前提不再成立。

### 與 `trade-cost-estimate`（另一位 agent 進行中）的關係

該 change 從 OKX tickers 取 `bidPx/bidSz/askPx/askSz`，並在「共同數量 `q` 大於該檔掛單量」時提示會吃到第二檔。OKX 的 `q` 在系統內是幣量，而 OKX 衍生品的掛單量依交易所慣例為**張數**（官方 tickers 文件未明示單位；同頁 `vol24h` 對衍生品為張數、`volCcy24h` 為幣量）。在 OKX 仍不可下單時，交易單頁不會出現 OKX 腿，所以該 change 不會觸發此問題；本 change 開放 OKX 後就會。因此本 change 負責：比較前把 OKX 掛單量乘 `ctVal`（或把 `q` 換成張數），並以測試鎖定。

## Goals / Non-Goals

**Goals:**
- 使用者在每個頁面上對 OKX 可做的事與 Binance、Bybit 相同（看帳戶、看持倉、手動下單 / 平倉、勾選候選、在交易單頁送出）。
- 所有 OKX 數量在頁面上以幣量為主、張數為輔，不會把張數誤當幣量。

**Non-Goals:**
- engine 送單邏輯、風控公式（不變）。
- OKX WebSocket 行情、OKX 專屬設定頁（帳戶模式切換等）。
- 「是否預設排除 OKX」以外的新風控欄位。

## Decisions

**D1　可下單集合 = 三所；是否交易 OKX 由既有的 `allowed_exchanges` 控制。**
不新增「OKX 開關」，沿用風控設定（`risk-config`：`allowed_exchanges` 預設三所）。
- 替代：可下單集合依「OKX 帳戶已連線」動態決定。會讓掃幣方向隨連線狀態跳動；連線狀態已由 engine 送單前檢查 fail closed，頁面不需重複。

**D2　手動下單的 OKX 數量以幣量輸入，換算張數後送出。**
`Quantity::okx_contracts(幣量, ctVal, lot)` 取整；顯示「N 張（≈ x BTC，≈ y USDT）」與合約設定頁一致；`ctVal` 未知則不可送出並顯示原因。
- 替代：直接輸入張數。與另外兩所操作不一致，也與交易單頁、合約設定頁的「幣量為主」不一致。

**D3　總覽 OKX 卡的資產與合約權益。**
資產列取 `balance.details[]` 的 `ccy`、`eq`（數量）、`eqUsd`（交易所估值）；合約權益：已用 = 全部 OKX 持倉 `imr` 之和（USD），可用 = `okx-signed-read` 的可用保證金。任一必要值缺失時沿用既有「無法估值」呈現，不當成 0。
- 替代：帳戶層 `totalEq`。只在跨幣種保證金模式有完整意義，且與另外兩所「逐資產加總」的呈現不一致。

**D4　持倉頁顯示張數與幣量兩欄資訊。**
OKX 列的數量顯示「x BTC（N 張）」；`ctVal` 未知時「N 張（無法換算）」且不參與以幣量為基礎的配對分組。

**D5　交易單頁的一檔掛單量比較在幣量上進行。**
OKX 掛單量 × `ctVal` 後才與 `q` 比較；`ctVal` 未知時不顯示「會吃到第二檔」提示而顯示「無法判斷」。

## Risks / Trade-offs

- [OKX 加入後，掃幣方向與達標的結果改變，自動模式可能開始選到 OKX 配對] → 遷移步驟明示；合併前使用者決定 `allowed_exchanges` 是否先排除 OKX（Open Question 1）。
- [OKX 帳戶模式不支援時，含 OKX 的候選在送單前才失敗] → 候選列顯示 OKX 帳戶狀態提示（不阻擋勾選，與另外兩所未連線時一致）；engine 依既有規則 fail closed。
- [三所兩兩配對讓掃幣計算量增加] → 每標的最多 3 組配對，計算在背景；以既有 `scan_view` 效能測試確認。

### OKX 特有陷阱

- 頁面上所有 OKX 數量來源（持倉 `pos`、委託 `sz`、成交 `accFillSz`、一檔 `askSz/bidSz`）都是張數，顯示或比較前必須乘 `ctVal`。
- `lotSz`、`minSz` 也是張數：手動下單的「低於最小下單量」要以張數判斷再換回幣量提示。
- OKX `availEq` 在跨幣種保證金模式是 USD；頁面標示「USD≈USDT」與 Bybit UTA 一致。

## Migration Plan

1. 合併前：使用者決定 OKX 是否預設在 `allowed_exchanges` 中（風控設定頁可改；設定值存在 DB，不受程式預設影響）。
2. 合併後首次啟動：OKX 未設定金鑰者，總覽與持倉頁的 OKX 顯示「未連線：NoKey」，手動下單 OKX 面板停用並顯示原因。
3. 回復：可下單集合改回兩所即恢復舊行為；無資料遷移。

## Open Questions

1. 開放 OKX 時，是否要在合併當下把既有使用者的 `allowed_exchanges` 改為排除 OKX，讓使用者手動開啟？（建議：不自動改，改由發布說明提醒。）
2. 掃幣頁「覆蓋」與方向欄寬是否需因三所配對而調整（實機目視）。

## 實作時發現

- 來自 `okx-execution-guards` 第三輪審查，列入 tasks 3.7：(a) 手動 OKX 單（含 reduce-only）須先換張數；(b) `recovery.rs` 對 OKX 的 `ctVal` 缺失不得退回 1；(c) `lotSz` 於持倉期間變大時的平倉檢查（僅文件化）；(d) 平倉因「無持倉」被拒時的警示文字需與「裸腿」區分。

## 實作時發現（合併 main 的 order-leverage-sync 之後）

- engine 的開倉單現在帶 `leverage`（`order-leverage-sync`），Binance / Bybit 的 executor 在送單前先設槓桿。OKX 的 set-leverage **尚未實作**：executor 對帶 `leverage` 的 OKX 開倉在任何請求之前回 `not_sent`（「OKX leverage sync not implemented yet」，測試涵蓋且斷言零請求），絕不呼叫任何 OKX set-leverage 端點，也絕不在槓桿未套用時送出 OKX 開倉。因此在 task 3.8 完成前，engine 自動進場含 OKX 腿的配對都會在送單階段失敗；手動單與實機探針的 OKX 腿不帶 `leverage`（探針已註明），不受影響。
