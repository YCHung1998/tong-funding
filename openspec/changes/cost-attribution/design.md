## Context

`core/src/pnl.rs` 已經算出 `price_ref`（只用參考價的價格損益）、`price_actual`、合併的 `slippage = price_ref − price_actual` 與費用。
開倉的 `expected_price` 來自 T−10 進場快照的 mark 價，平倉的參考價來自平倉前的單一標的重抓（`reference_source = refetch_before_close`，`app/src/engine/actor.rs`）。
所以「持倉漂移」就是既有的 `price_ref`，缺的只是：(1) 把 `slippage` 拆成進場與出場、(2) 以每腿名目 % 表示、(3) 對照模型假設的報表、(4) 機會型態標示。

基準觀察（`funding.db`，17 筆已平倉，用原始事件還原；平均／中位，每腿名目 %）：
進場滑價 −0.07／−0.04、持倉漂移 −0.24／−0.15、出場滑價 −0.18／−0.16、手續費 0.22／0.19；12/17 漂移為負。
注意：這 17 筆含 10-14 的壓力測試（刻意放寬門檻），demo 的訂單簿與正式環境不同，且進場參考價與平倉參考價是否同為 mark 價尚未確認（見 Open Questions）。

## Goals / Non-Goals

**Goals:**
- 每筆 PnL 事件自帶可驗證的成本拆解（恆等式、未知不當 0）。
- 一份唯讀報表，把實際成本分布與目前模型假設並排，給出建議值。
- 每個機會都標示「錢從哪來」：週期錯位或費率差。

**Non-Goals:**
- 不改 Net Edge 公式、不套用任何建議值、不改預設值。
- 不做深度（多檔）滑價估算（`trade-cost-estimate` 範圍，等量測後再決定）。
- 不改 PnL 頁的畫面；不新增事件類型或資料表。
- 不處理單腿風險與熔斷（另一個 change）。

## Decisions

1. **拆解放進既有 PnL payload，不新增事件。** PnL 事件已是事實紀錄且只增不改；多一個 `attribution` 欄位不影響既有消費者（`record` 的去重比對只看 `breakdown`/`status`/`reasons`/`expected_vs_actual`，而 `attribution` 由同一份輸入導出，結果一致）。
   - 替代：新事件 `PAIR_COST_ATTRIBUTION` → 多一種事件要維護、重算時要同步，且與 PnL 可能不一致。
2. **純函式放 core，輸入沿用 `PairInput`。** 不重算價格損益，直接分 action 累加每筆成交的 `dir × (ref − act) × q`，並以測試固定恆等式。
3. **漂移直接取 `price_ref`。** 不另發明定義，與既有 PnL 一致；報表和畫面說明「兩所參考價在持倉期間的相對變化」。
4. **型態用一個可解釋的閾值（一半）。** `rate_gap_income < 0.5 × model_income` 為週期錯位。閾值寫成常數並記錄「只用於顯示」，之後可依資料調整而不影響交易。
   - 替代：只用「兩腿週期是否不同」→ 過粗，KAIA 這類週期不同但費率差真實存在的會被錯分。
5. **報表以 Python 寫在 `issue/tools/`（專案既有慣例）。** 優先讀 `attribution`；舊事件由原始事件還原並標「還原值」。為避免兩套邏輯分岔，測試必須用同一筆合成配對驗證兩條路徑的結果一致。
6. **建議值只輸出不套用。** `est_slippage_pct` = 總滑價（進場＋出場）分位數 ÷ 4；`safety_margin_pct` = 不利漂移（漂移為負時的絕對值）分位數。預設顯示 p50 / p75 / p90，由使用者決定用哪一個。

## Risks / Trade-offs

- **參考價來源不一致會讓「漂移」帶偏差**（開倉 mark、平倉重抓價）→ 實作時先查清楚並記錄於本文件；若不同源，`attribution` 加 `reference_sources` 欄位如實標示，報表在來源不同時加警示。
- **樣本小且含壓力測試**（17 筆）→ 報表強制標示樣本數與「樣本不足」，建議值不自動套用。
- **demo 數字不能代表正式環境** → 報表開頭固定說明；本 change 的價值是流程與可見性，不是具體數字。
- **新欄位讓 PnL 事件變大** → 欄位僅十餘個數字，可忽略。

## Open Questions

- 平倉前重抓的價格是 mark、last 還是 ticker？與進場快照的 `expected_price`（mark）是否同源？實作第一步查清楚並寫回本節。
- 型態閾值 0.5 是否合適？先以資料觀察，不在本 change 調整。

## Payload 形狀（`attribution`）

數值皆為十進位字串；`null` = 未知。符號：`funding`、`drift`、`net` 為損益（正＝賺）；`entry_slippage`、`exit_slippage`、`fees` 為成本（正＝付出／不利）。

```json
{
  "notional_per_leg": "995.0",
  "funding":        { "usdt": "4.0064", "pct": "0.4026" },
  "drift":          { "usdt": "-2.3542", "pct": "-0.2366" },
  "entry_slippage": { "usdt": "0.3000", "pct": "0.0301" },
  "exit_slippage":  { "usdt": "0.9000", "pct": "0.0905" },
  "fees":           { "usdt": "1.8959", "pct": "0.1905" },
  "net":            { "usdt": "-1.1503", "pct": "-0.1156" },
  "unknown": [],
  "reference_sources": { "open": "mark(entry_snapshot)", "close": "refetch_before_close" }
}
```
