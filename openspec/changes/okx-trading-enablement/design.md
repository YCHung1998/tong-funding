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

## 抗辯修正（enablement）

- F1：OKX executor 才會做的拒絕（數量守衛：名目上限 / lotSz / 預期幣量 / ctVal、limits 未載入、標記價過舊）改在**送任何一腿之前**對兩腿檢查：`Executor::prepare_open`（DemoExecutor 以同一組 `local_checks` + `gate` 實作）。失敗 = 兩腿都不送，配對以 `nothing sent` 取消並帶原因。腿的名目上限來自該配對自己的進場 notional（`OrderRequest.notional_usdt` × 2，`OKX_LEG_CAP_FACTOR`），不再受之後調低的合約範本影響；手動單沒有配對 notional，仍用範本 × 2。executor 內的同一檢查保留為第二道防線。
- F2：槓桿屏障（所有交易所）：`prepare_open` 先對每腿做本地檢查與 gate（時鐘、單向模式、OKX 示範金鑰證明），再對**每一腿**設槓桿；全部成功才送單，且送單不重複設槓桿。任一腿被拒 / 限流 / 逾時 = 不送任何訂單（設槓桿不產生曝險）。Binance / Bybit 的「槓桿未變更」碼語意不變。
- F3：OKX set-leverage 成功需回覆 `data[0]` 的 `lever` 等於所求、`mgnMode` 為 `cross`、`instId` 吻合，否則視為失敗、不送單。
- F4：`actor.rs` 對 OKX 缺 `ctVal` 改為錯誤（與 recovery 一致）。
- F5：OKX 在 `allowed_exchanges` 內時，OKX 帳戶斷線 / 時鐘警示與其他兩所相同；`demo_keys` 在 OKX 被允許時要求 OKX 金鑰與 passphrase（否則不可切換 EXCHANGE_DEMO，附原因）。
- F6：掃幣頁每列依自己的配對設定判斷；缺 OKX 手續費只讓含 OKX 的列顯示「未設定」，Binance–Bybit 列不受影響（全域 `decidable` 旗標與排序仍以全部配對為準，行為不變）。
- F7：OKX 帳戶不顯示合約權益列（與 live 一致，避免與錢包資產重複計入）；測試固定值已對齊。
- 簡化（無行為變更）：`UiSnapshot::okx_ct_val`、`PositionRow` 去掉重複欄位、刪除未使用的 `close_prefill(&AccountPosition)`、`OkxOrderClient::order_gate`、`okx_account_data` 接收 `incomplete`、帳戶輪詢沿用市場迴圈的 limits 表、`TRADABLE/ACCOUNT_EXCHANGES = Exchange::ALL`。

## 抗辯修正（enablement 第二輪）

- G1：被屏障核准的腿以 `OrderRequest.prepared = true` 標記（引擎在 `OpenPrepared` Ok 時設定；取代 executor 內共享的 `prepared` 集合，沒有可洩漏的共享可變狀態）。已核准的腿送單時**略過**會隨時間改變的軟性檢查：模式重讀 / TTL、back-off、標記價新鮮度、limits / 範本上限、槓桿；只保留硬性停止：OKX 閂鎖、示範金鑰證明、傳輸層的示範旗標規則（無條件）。這修掉「屏障通過後模式快取過期且重讀 503，Binance 已成交而 OKX 不送」的裸腿重現。唯一接受的殘餘風險：屏障與送單之間閂鎖跳脫（代表環境不符，OKX 本來就會拒絕）；此時該腿以 `not_sent` 回覆，原因明說「屏障已通過，對側腿可能已送出且為裸腿」，引擎將該配對落入 PARTIAL_FAILURE，既有的人工處理警示會點名它。
- G2：回歸測試涵蓋模式 TTL 過期 + 503、屏障後開始 back-off、limits 消失（標記價過期 / 範本不可讀）皆兩腿都送出；閂鎖殘餘有文字測試；另有以真實 `DemoExecutor`（假傳輸）經真實引擎跑完屏障 + 送單的流程測試（`exchange/execution/engine_flow_tests.rs`，引擎目錄不得出現網路層名稱，故測試放在 exchange 側）。
- G3：`OpenPrepared` Ok 時再檢查 kill switch / 停機 / 對帳未完成與進場視窗，失敗即 `nothing sent`。
- G4：`on_tick` 對卡在屏障超過 `order_timeout_seconds` 的配對以「pre-send barrier timed out」取消（nothing sent）；之後才回來的屏障結果被忽略。
- G5：配對處於 ORDER_SUBMIT（含屏障）時，引擎拒絕同交易所同標的的手動**開倉**單（避免手動 set-leverage 改掉配對核准的槓桿）；平倉永不被擋。
- G6：set-leverage 回覆必須帶 `instId` 且吻合（OKX 文件的回覆資料含 `instId`、`lever`、`mgnMode`、`posSide`；仍屬依文件、未實機驗證，列入 4.2）。
- G7：`demo_keys` 在 `allowed_exchanges` 改變時於下一個 2 秒的設定讀取就重算（`DemoKeysCadence`），其餘維持每 15 次的慢節奏。
