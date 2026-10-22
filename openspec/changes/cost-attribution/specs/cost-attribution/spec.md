## ADDED Requirements

### Requirement: 配對 PnL 的成本拆解

系統 SHALL 在每個配對的 `PAIR_PNL_COMPUTED` 與 `PAIR_PNL_RECOMPUTED` 事件 payload 中寫入 `attribution`，把價格相關損益拆成三段，並與資金費、手續費並列。
以 `dir` 表示方向（多腿：開倉 −1、平倉 +1；空腿相反），`q` 為成交數量，`ref_open`、`ref_close` 為該腿記錄的開倉與平倉參考價，`act_open`、`act_close` 為平均成交價：

- **進場滑價（entry_slippage）** = Σ 開倉成交 `dir × (ref_open − act_open) × q`（正值＝不利，與既有 `slippage` 同號）。
- **出場滑價（exit_slippage）** = Σ 平倉成交 `dir × (ref_close − act_close) × q`（正值＝不利）。
- **持倉漂移（drift）** = 既有的 `price_ref` 元件，即只用參考價算出的價格損益（兩腿加總；等於兩所參考價在持倉期間的相對變化）。
- 恆等式：`entry_slippage + exit_slippage = slippage`；`funding + drift − entry_slippage − exit_slippage − opening_fee − closing_fee = net`（與既有 `PnlBreakdown` 的 `net` 一致）。
- 每項同時給出 USDT 與「每腿名目 %」；每腿名目 = 兩腿開倉成交金額（`q × act_open`）的平均。
- 任何一項所需資料缺少（參考價、成交價、費用），該項 SHALL 為未知（`null`），SHALL NOT 以 0 代替，且不得影響其他項目的呈現；`attribution` 同時列出未知項目名稱。
- 此欄位 SHALL 只增加資訊，不改變既有 `breakdown`、`status`、`slippage` 的值。

#### Scenario: 恆等式成立

- **WHEN** 多腿開倉參考價 100、成交價 100.1、平倉參考價 100.5、成交價 100.4，數量 10；空腿開倉參考價 100.2、成交價 100.1、平倉參考價 100.6、成交價 100.7，數量 10
- **THEN** 多腿進場滑價 1.0、出場滑價 1.0；空腿進場滑價 1.0、出場滑價 1.0；進場滑價合計 + 出場滑價合計等於既有 `slippage` 合計；漂移等於既有 `price_ref`

#### Scenario: 參考價缺少時為未知

- **WHEN** 某腿平倉成交沒有記錄參考價
- **THEN** 出場滑價與漂移為 `null` 並列於未知項目；進場滑價、資金費、手續費仍照常輸出；`slippage` 與 `breakdown` 與目前行為相同

#### Scenario: 百分比以每腿名目計算

- **WHEN** 兩腿開倉成交金額分別為 1000 與 990，漂移為 −2.0 USDT
- **THEN** 每腿名目為 995，漂移為 −0.2010%（四捨五入顯示，內部保留完整精度）

### Requirement: 校準報表為唯讀且只給建議

`issue/tools/cost_report.py` SHALL 以唯讀方式（`mode=ro`）開啟資料庫，輸出：逐筆拆解表、各項佔總成本的比例、進場＋出場滑價與不利漂移的 p50 / p75 / p90，以及建議的 `est_slippage_pct`（總滑價分位數 ÷ 4，因 Net Edge 以 4 次成交計）與 `safety_margin_pct`（不利漂移分位數）。
報表 SHALL NOT 寫入資料庫、SHALL NOT 修改任何設定。樣本數少於 30 筆時 SHALL 明確標示「樣本不足，僅供參考」，並顯示樣本數。
舊事件沒有 `attribution` 時，報表 SHALL 由原始事件（進場快照、`ORDER_FILL`、平倉參考價）還原，並在該列標示「還原值」；還原結果與新欄位對同一筆配對 SHALL 一致。

#### Scenario: 樣本不足

- **WHEN** 資料庫只有 17 筆已平倉配對
- **THEN** 報表顯示 n=17 與「樣本不足，僅供參考」，仍輸出分位數與建議值

#### Scenario: 不寫入資料庫

- **WHEN** 執行報表
- **THEN** 資料庫檔案的內容與修改時間不變

#### Scenario: 舊資料還原與新欄位一致

- **WHEN** 同一筆配對同時有 `attribution` 與可還原的原始事件
- **THEN** 兩者的進場滑價、出場滑價、漂移相等
