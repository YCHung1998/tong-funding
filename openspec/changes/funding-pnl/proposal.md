## Why

整個策略的獲利來源是 funding，但 Figma 持倉頁只有價差 PnL，並註明「未扣手續費與資金費」。
沒有 Funding PnL，監控工具看不到策略真正的成效，Net Edge 的估計也無法用實際結果校正。
Python 版（`mvp-python`）沒有任何 funding 流水或 PnL 的實作（SYSTEM_SPEC §31–§33 只有規格），因此本 change 是新功能，不是搬移。

## What Changes

- 從各交易所取得 funding 收付流水：Binance `GET /fapi/v1/income`（`incomeType=FUNDING_FEE`）、Bybit `GET /v5/account/transaction-log`（`type=SETTLEMENT`）；端點與欄位已對公開文件查證，**對真實 demo 帳戶的回傳格式尚未驗證**，task 1.1 先驗證再寫解析。
- 流水寫入不可變事件表，以交易所端 id 去重。
- 每腿與每組配對的 PnL 拆解：Funding PnL + 價差 PnL − 開倉手續費 − 平倉手續費 − 滑價 − 其他成本 = Net PnL（SYSTEM_SPEC §31）；另含成交比與滑價分析（§32、§33）。
- 與交易所流水對帳；差異以事件記錄並警示。
- 預期（下單當時 Net Edge 的估計）對實際可逐項比較。
- 持倉頁新增「Funding 收到」欄（`ui-readonly-pages` 已留位置）與結算時間軸。
- 修改 `pair-lifecycle`：`FINALIZED` 增加「PnL 已計算」的前置條件（對應 SYSTEM_SPEC §29 的 COMPLETED 定義）。

## Capabilities

### New Capabilities
- `funding-history-fetch`: 各所 funding 流水的取得、時間窗與分頁、寫入事件表與去重。
- `pnl-accounting`: PnL 拆解、成交比與滑價、狀態與不完整判定、對帳、預期對實際、PnL 事件。
- `settlement-timeline`: 持倉頁 Funding 收到欄、結算時間軸、預期對實際比較面板與警示呈現。

### Modified Capabilities
- `pair-lifecycle`: `FINALIZED` 須確認 PnL 已計算。（此 capability 於 `core-domain-and-fixtures` 封存後才存在，見 `design.md` 的依賴與封存順序。）

## Impact

- 依賴 `exchange-demo-execution`（有真實成交才有流水與成交明細）與 `engine-simulation`（下單當時的預期價格與 Net Edge 快照須寫入事件，見 `design.md` D6）。
- 新增簽名 GET 端點（income / transaction-log），沿用 `exchange-readonly-adapters` 的簽名、校時、限流退避與端點寫死 demo/testnet 的機制。
- `store-sqlite` 的事件表新增一個 migration：以 `FUNDING_LEDGER_ENTRY` 事件的去重鍵建立部分唯一索引（不更動既有表欄位）。
- `core` 新增純函式模組（PnL 拆解、滑價、成交比、對帳比較）。
- `SIMULATION` 沒有真實倉位，因此不產生 funding 流水與實際 PnL。
