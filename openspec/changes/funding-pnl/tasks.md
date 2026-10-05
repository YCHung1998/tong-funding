> 凡改邏輯的 task，都要先寫測試並確認紅燈，再實作轉綠，並回報實際指令與測試檔路徑。
> 純計算放 `core`（`cargo test -p core pnl`）；取得與寫入放 `app`。任何「未驗證」的交易所行為，以 task 1.1 的真實 demo 回應為準，不憑文件推測。

## 1. 流水取得

- [ ] 1.1 **先驗證再寫解析**：用 demo 金鑰對 Binance `GET /fapi/v1/income`（`incomeType=FUNDING_FEE`）與 Bybit `GET /v5/account/transaction-log`（`type=SETTLEMENT`）各做一次**唯讀**簽名 GET，把去敏（移除金鑰與帳戶識別）後的回應存為 `app/tests/fixtures/funding/*.json`，並把 `design.md` 的「驗證紀錄」表逐項填入（欄位名稱、金額正負號、時間單位、id 是否唯一、是否有 funding 流水、單頁上限、時間窗上限）。驗收：表格每一列都有「已驗證／與文件不符」結論；與文件不符處同步修正 spec 與 design
- [ ] 1.2 解析為標準化 `FundingLedgerEntry`（交易所、標的、金額〔收到為正〕、幣別、結算時間、交易所端 id、原始回應）。先對 1.1 的 fixtures 寫測試（Bybit `funding = -0.003676` 為支付、正負號各一案、Binance `tranId` 組成去重鍵）並確認紅燈；`cargo test -p app funding_parse`
- [ ] 1.3 時間窗切分（每窗 ≤ 7 天）、分頁至耗盡、無法確認耗盡即失敗（失敗即封閉）、429 退避。以 mock 回應測試：10 天範圍切成兩窗、有 `nextPageCursor` 就繼續、任一頁失敗整體標示不完整；先紅燈再轉綠
- [ ] 1.4 事件表寫入與去重：新增 `FUNDING_LEDGER_ENTRY` 去重鍵的部分唯一索引 migration；重複抓取不重複寫入；同 id 不同金額寫 `FUNDING_LEDGER_CONFLICT` 且不覆寫。測試：重複抓取筆數不變、UPDATE/DELETE 仍被 trigger 擋、衝突案例；`cargo test -p app funding_ledger_store`

## 2. 計算與對帳

- [ ] 2.1 `core` PnL 拆解純函式：Funding PnL、價差 PnL（以參考價）、開／平倉手續費、滑價、其他成本、Net PnL，及恆等式。測試先寫手算案例（本 change spec 的 −0.82 USDT 案例）、單腿配對、費用幣別非 USDT 時標記不完整；`cargo test -p core pnl_breakdown`
- [ ] 2.2 `core` 成交比與滑價（Long／Short 正負號、adverse 為正）與歸屬規則（交易所＋標的＋時間窗）。測試：997.42 / 1000 = 0.99742、long 與 short 滑價案例、窗界邊界（開倉成交時刻之前的結算不計入）
- [ ] 2.3 PnL 狀態判定（`COMPLETE` / `INCOMPLETE` 與原因清單）與預期結算次數推算（重用 `funding-observation` 的結算時間）。測試：缺少一次結算流水 → `INCOMPLETE`（不是 0）、`SIMULATION` 配對不產生 PnL
- [ ] 2.4 對帳：重新抓取配對時間窗內流水，與已寫入合計逐腿比較，寫 `PNL_RECONCILIATION` 事件（OK 或 MISMATCH），MISMATCH 觸發警示。測試：一致、金額不同、交易所多一筆、本地多一筆
- [ ] 2.5 預期對實際：由下單當時的 Net Edge 快照與實際分量逐項計算差異與百分比；無快照時顯示「無預期快照」。測試：手算案例與缺快照案例

## 3. 狀態機與事件

- [ ] 3.1 `pair-lifecycle`：`FINALIZED` 須同時附帶「已平倉確認」與「PnL 已計算」確認；PnL 結果寫 `PAIR_PNL_COMPUTED`（重算寫 `PAIR_PNL_RECOMPUTED`，以最新為準）。測試先寫：缺 PnL 確認被拒、缺平倉確認被拒、兩者齊全轉為 `FINALIZED`、人工平倉的配對同樣需要；紅燈後實作；`cargo test -p core pair_lifecycle`

## 4. 呈現

- [ ] 4.1 持倉頁「Funding 收到」欄、結算時間軸、預期對實際面板、警示呈現的 view-model 與畫面。測試：欄位未取得顯示「—」而非 0、時間軸排序與缺少標示；逐項截圖對照 Figma 持倉頁並列出差異

## 5. 驗證

- [ ] 5.1 在 demo 帳戶走完一組配對跨過至少一次結算：確認流水取得、去重、PnL 拆解與對帳；把實際數字與預期對實際比較結果記入 `design.md` 的「驗證紀錄」；回報實際指令與測試檔路徑
