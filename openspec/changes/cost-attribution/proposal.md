## Why

17 筆已平倉實單（demo，`funding.db`）合計：資金費 +61.21、價格損益 −75.64、手續費 −34.98、淨 −49.40 USDT。
價差損益的大小與資金費同量級，但 Net Edge 只用一個固定的 `est_slippage_pct`（那批設定是 0.005%，即總滑價約 0.02%），完全沒有「持倉期間兩所價差變化」這一項。
拆開看（每腿名目的 %，17 筆平均）：進場滑價 −0.07、**持倉漂移 −0.24**、出場滑價 −0.18、手續費 −0.22，合計 −0.70；資金費平均：模型預期（T−10 正式費率）+0.47、demo 實收 +0.34。15/17 筆的漂移為負（只有 SIREN、ERA 為正），不像隨機。

目前也無法回答「這筆錢是從哪來的」：7/8 筆進場時兩所每小時費率差約 0，賺的其實是結算週期錯位，不是費率差，但紀錄與畫面不區分。
先把「實際成本」拆開、可重複量測，才有依據調整滑價與安全邊際；demo 的訂單簿與正式環境不同，所以本 change **只做量測與標示，不自動改任何設定值**。

## What Changes

- **成本拆解**：每筆配對的 PnL 事件新增 `attribution`，只補既有 `breakdown` 缺的兩個值：進場滑價與每腿名目。出場滑價（= `slippage` − 進場滑價）、持倉漂移（= `price_ref`，兩所參考價之間的變化）、手續費、資金費由 `breakdown` 推導。資料不全時為「未知」，不寫 0。
- **機會型態標示**：新增「週期錯位型」與「費率差型」兩種型態與每小時費率差；寫進進場快照，並在掃幣頁的公式區塊旁顯示。
- **校準報表（唯讀）**：`issue/tools/cost_report.py` 讀取資料庫，輸出逐筆表、各項不利成本佔比、實際成本分布（p50 / p75 / p90），並**建議**（不套用）`est_slippage_pct` 與 `safety_margin_pct`，附上每組建議值的樣本內回測；如實標示樣本不足、略過的配對，以及「demo 成交與正式環境參考價不在同一個市場」的限制。
- 不改 Net Edge 公式、不改任何風控預設值、不改下單流程。

## Capabilities

### New Capabilities
- `cost-attribution`: 配對 PnL 的成本拆解（進場滑價、持倉漂移、出場滑價、手續費、資金費）與恆等式、未知處理、校準報表。
- `edge-kind`: 機會型態（週期錯位型／費率差型）的判定規則與每小時費率差。

### Modified Capabilities
- `scanner-threshold-breakdown`: 掃幣頁顯示每個候選的型態標籤。

## Impact

- `core/src/`：新增 `cost_attribution.rs`、`edge_kind.rs`（純函式與測試）。
- `app/src/funding/pnl_record.rs`：`pnl_payload` 增加 `attribution`。
- `app/src/engine/actor.rs`（`entry_snapshot_json`）：進場快照的 `net_edge` 增加型態與每小時費率差。
- `app/src/ui/`：掃幣頁型態標籤（view-model 與測試）。
- `issue/tools/cost_report.py` 與測試。
- 不新增事件類型、不改資料庫結構；舊事件沒有 `attribution`，報表以原始事件還原那兩個值並標示「還原值」，其餘一律讀既有 `breakdown`。
