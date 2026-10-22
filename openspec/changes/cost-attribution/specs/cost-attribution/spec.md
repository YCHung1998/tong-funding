## ADDED Requirements

### Requirement: 配對 PnL 的成本拆解

系統 SHALL 在每個配對的 `PAIR_PNL_COMPUTED` 與 `PAIR_PNL_RECOMPUTED` 事件 payload 中寫入 `attribution`，只包含既有 `breakdown` 沒有的兩個值（十進位字串，未知為 `null`）：

- **進場滑價（`entry_slippage`，USDT）** = Σ 各腿開倉成交 `dir × (ref_open − act_open) × q`（多腿開倉 `dir = −1`、空腿開倉 `dir = +1`；正值＝不利，與既有 `slippage` 同號）。`ref_open` 為該腿開倉成交記錄的參考價（`expected_price`）。
- **每腿名目（`notional_per_leg`，USDT）** = 有開倉成交的各腿開倉成交金額（`q × act_open`）的平均。

其餘拆解項目 SHALL 由同一筆事件的 `breakdown` 推導，不重複儲存：

- **出場滑價** = `breakdown.total.slippage − entry_slippage`。
- **持倉漂移** = `breakdown.total.price_ref`（只用參考價算的價格損益，兩腿加總）。
- **手續費** = `opening_fee + closing_fee`；**資金費**、**淨額**直接取 `funding`、`net`。
- 某項所依賴的元件列在 `breakdown.missing` 時，該項為未知；出場滑價在 `slippage` 或 `entry_slippage` 未知時為未知。

未知規則：

- 沒有任何開倉成交的腿貢獻 0（它沒有交易），不使未知。
- 任一開倉成交缺參考價、缺成交價、或數量不是幣數（OKX 未換算），`entry_slippage` 與 `notional_per_leg` 為 `null`，SHALL NOT 以 0 代替。
- 計算溢位時為 `null`，SHALL NOT panic。
- 此欄位 SHALL 只增加資訊，不改變既有 `breakdown`、`status`、`slippage` 的值與事件去重行為。

#### Scenario: 進場滑價與推導的出場滑價

- **WHEN** 多腿開倉參考價 100、成交價 100.1、平倉參考價 100.5、成交價 100.4，數量 10；空腿開倉參考價 100.2、成交價 100.1、平倉參考價 100.6、成交價 100.7，數量 10
- **THEN** `entry_slippage` 為 2.0、`notional_per_leg` 為 1001；既有 `slippage` 為 4.0，所以推導的出場滑價為 2.0；漂移等於既有 `price_ref`（+1.0）

#### Scenario: 部分平倉時進場已知、出場未知

- **WHEN** 多腿開倉 10、只記錄到平倉 6（既有 `breakdown` 把 `Slippage`、`PriceRef` 標為缺漏）
- **THEN** `entry_slippage` 照常計算；推導的出場滑價與漂移為未知，不是 0

#### Scenario: 只有一腿成交的配對

- **WHEN** 一腿下單失敗沒有任何成交，另一腿已開已平
- **THEN** `entry_slippage` 只由有成交的那腿計算（已知）；`notional_per_leg` 為那一腿的開倉成交金額

#### Scenario: 參考價缺少時為未知

- **WHEN** 某腿開倉成交沒有記錄參考價
- **THEN** `entry_slippage` 與 `notional_per_leg` 為 `null`；`breakdown` 與目前行為相同

### Requirement: 校準報表為唯讀、如實標示限制、只給建議

`issue/tools/cost_report.py` SHALL 以唯讀方式開啟資料庫，輸出：逐筆拆解表（含「提前出場」標記）、各項不利成本佔比、成本分位數（p50 / p75 / p90）、建議的 `est_slippage_pct`（總滑價分位數 ÷ 4）與 `safety_margin_pct`（不利漂移分位數），以及每組建議值的**樣本內回測**。

- **唯讀**：路徑 SHALL 以正確編碼的 `file:` URI 開啟（`mode=ro`）。只有在錯誤為「無法開啟資料庫檔」且 `-wal`、`-shm` 都不存在（App 已關閉）時，才改以 `immutable=1` 讀取，並在報表開頭標示；讀取前後主檔大小或修改時間改變、或 `-wal`／`-shm` 出現時 SHALL 中止並要求重跑。其他錯誤 SHALL 直接報錯。報表 SHALL NOT 寫入資料庫或修改任何設定。
- **來源**：只使用每個配對**最新**一筆 PnL 事件；該事件沒有 `attribution` 時，由原始事件還原 `entry_slippage` 與 `notional_per_leg`（最後一筆進場快照的 `expected_price`；開倉成交依 `client_order_id` 取各單最後一筆累計成交後加總；缺 `expected_price` 或 OKX 腿為未知，不以 mark 價代替），該列標示「還原值」；其他項目一律由 `breakdown` 推導。
- **完整性**：SHALL 列出被略過的配對與原因，並顯示「FINALIZED 總數 / 實際使用筆數」。樣本數少於 30 筆時 SHALL 標示「樣本不足，僅供參考」。
- **佔比**：每項只計不利的部分（逐筆 `max(0, 成本)`、`max(0, −漂移)`），與表頭說明一致。
- **市場限制**：報表開頭 SHALL 說明參考價是正式環境 mark 價；demo 資料庫的成交在 demo 市場，所以滑價含 demo 與正式盤的價差，不可當作正式環境滑價。
- **樣本內回測**：對每組建議值，以各配對進場快照的資金費收入、手續費、名目與門檻重算 Net Edge，列出仍會進場的筆數、勝負筆數與實際淨損益合計（%）；淨損益仍為負時 SHALL 明確標示。

#### Scenario: 樣本不足

- **WHEN** 資料庫只有 17 筆已平倉配對
- **THEN** 報表顯示 n=17 與「樣本不足，僅供參考」，仍輸出分位數、建議值與樣本內回測

#### Scenario: 不寫入資料庫

- **WHEN** 執行報表（App 執行中或已關閉）
- **THEN** 資料庫檔案的內容不變；App 已關閉時報表開頭標示以快照方式讀取

#### Scenario: 不使用過期的拆解

- **WHEN** 最新一筆 PnL 事件沒有 `attribution`，較舊的一筆有
- **THEN** 該配對走還原路徑並標示「還原值」，不使用較舊的 `attribution`

#### Scenario: 建議值在樣本內仍為負

- **WHEN** 套用某組建議值後仍會進場的配對，實際淨損益合計為負
- **THEN** 報表在該組建議值旁標示「樣本內仍為負」
