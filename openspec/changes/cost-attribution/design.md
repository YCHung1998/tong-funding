## Context

`core/src/pnl.rs` 已經算出 `price_ref`（只用參考價的價格損益）、`price_actual`、合併的 `slippage = price_ref − price_actual` 與費用，並寫進每筆 PnL 事件的 `breakdown`。
所以「持倉漂移」就是既有的 `price_ref`；真正缺的只有兩個值：**進場滑價**（才能把 `slippage` 拆成進場與出場）與**每腿名目**（才能換算成 %）。

**參考價與成交的市場（1.1 查證，經審查修正）：**
- 開倉與平倉的參考價都是**正式環境的 mark 價**：開倉 = 下單前重抓（`actor.rs:1394` → `entry_snapshot.expected_price`，`actor.rs:3117`）；平倉 = 平倉前重抓（`actor.rs:1949-1953` → `reference_price`，`actor.rs:2111`）；提前出場 = 觸發時的價格（`actor.rs:1182`、`1216`）。三者都經 `PublicMarketData::refetch`（`ui/wiring.rs:52-60`）→ `fresh_quote`，`price = mark_price`（`ui/wiring.rs:28-30`），主機是正式環境（`exchange/public/endpoints.rs:5,12`）。
- 成交：Demo 在 demo 主機（`exchange/signed/endpoints.rs:24-25`），Live 在正式環境。
- 所以兩個參考價彼此同源（漂移是正式環境兩所 mark 價的相對變化），但**在 Demo，滑價 = demo 成交價 − 正式 mark 價，含 demo 與正式盤的價差**，不能當成正式環境滑價。審查量到 demo 成交對正式 mark 的平均偏差：Binance 0.16～0.18%（最大 0.71%），Bybit 0.09～0.15%。Live 的參考價與成交才是同一個市場。

**基準（`funding.db`，17 筆已平倉，報表輸出）：**
平均（每腿名目 %）：進場滑價 0.067、漂移 −0.235、出場滑價 0.177、手續費 0.218；15/17 漂移為負。
17 筆合計（USDT）：資金費 +61.21、價格損益 −75.64、手續費 −34.98、淨 −49.40。
限制：自 10-14 起門檻幾乎為 0（0.0021% 或 0.00021%）、`safety_margin` 0、`est_slippage` 0.005%，所以樣本是「幾乎所有極端費率機會」；10 個幣、11 個結算時點；4 筆有提前出場（佔漂移總和 46%）。

## Goals / Non-Goals

**Goals:**
- 每筆 PnL 事件補上拆解所缺的兩個值，其餘由 `breakdown` 推導（單一來源、未知不當 0）。
- 一份唯讀報表，如實標示樣本與市場限制，把實際成本分布、建議值與**建議值的樣本內回測**並排。
- 每個機會標示「錢從哪來」：週期錯位或費率差。

**Non-Goals:**
- 不改 Net Edge 公式、不套用任何建議值、不改預設值。
- 不做深度（多檔）滑價估算、不記錄正式環境的 T±60 秒價格路徑（區分「除息式回吐」等假設所需的資料，另議）。
- 不改 PnL 頁的畫面；不新增事件類型或資料表。
- 不處理單腿風險、熔斷與提前出場的觸發價問題（另一個 change）。

## Decisions

1. **`attribution` 只存 `breakdown` 沒有的兩個值（`entry_slippage`、`notional_per_leg`）。** 出場滑價 = `slippage − entry_slippage`，漂移 = `price_ref`，手續費、資金費、淨額直接取 `breakdown`；是否未知看 `breakdown.missing`。
   - 替代（最初版本）：把每項都存成 `{usdt, pct}` 並加 `unknown`、`reference_sources` → 與 `breakdown` 重複、pct 可由名目推出、`reference_sources` 是寫死常數且沒有讀取者；審查（simplifier）判定過度設計。
   - 替代：新事件類型 → 多一種事件要維護，且可能與 PnL 不一致。
2. **未知規則跟著資料走。** 沒有開倉成交的腿貢獻 0（單腿配對的進場滑價仍已知）；有開倉成交但缺參考價、缺成交價或 OKX 未換算時為未知；部分平倉時 `breakdown` 已把 `Slippage`、`PriceRef` 標為缺漏，所以推導的出場滑價與漂移為未知，進場滑價仍已知。運算用 checked 版本，溢位為未知、不 panic（`classify_edge` 在送單路徑上、`attribute` 在 PnL gate 內，actor panic 不會自動重啟）。
3. **漂移直接取 `price_ref`。** 不另發明定義；報表說明它是「正式環境兩所 mark 價在持倉期間的相對變化」。
4. **型態閾值 0.5，費率差收入以 `max(h_L, h_S)` 計。** 比較「這次單一結算的收入」與「持有一個完整較長週期時每小時費率差能賺的」；後者不到前者一半才算週期錯位。
   - 替代（最初版本）：以 `min(h)` 計 → 會把「長週期那所費率大、短週期那所接近 0」（例：BIRB Binance −0.951%/4h 對 Bybit −0.0044%/1h，持有 4h 仍保留 99% 收入）誤標為週期錯位；審查在 17 筆中找到 4 筆。
   - 替代：只看兩腿週期是否不同 → 更粗。
5. **報表只還原舊事件缺的兩個值，其餘讀 `breakdown`。** 還原規則與引擎一致：最後一筆進場快照的 `expected_price`；開倉成交依 `client_order_id` 取各單最後一筆累計成交再加總；缺 `expected_price` 或 OKX 腿為未知（不以 mark 價代替）。舊的 `breakdown` 與目前 Rust 重算的結果在 17 筆上完全一致（已逐筆比對）。
6. **建議值只輸出不套用，並附樣本內回測。** `est_slippage_pct` = 總滑價分位數 ÷ 4；`safety_margin_pct` = 不利漂移分位數。每組都以進場快照重算 Net Edge，列出仍會進場的筆數與其實際淨損益；審查已指出在這 17 筆上套用 p50 後仍會進場的 8 筆淨損益合計為負，所以報表必須把這個結果放在建議值旁邊。

## Risks / Trade-offs

- **[Demo 的滑價含 demo 與正式盤的價差]** → 報表開頭固定說明；只有 Live 資料可以用來校準滑價。漂移是正式環境 mark 價的變化，受此影響較小，但可能受 mark 價公式（例如含「費率 × 距下次結算時間」的項）在結算瞬間的跳動影響——未驗證。
- **[提前出場造成約 10 秒單腿曝險，且 Demo 觸發用正式 mark 對 demo 開倉均價]** → 報表標記提前出場的配對；觸發價問題與單腿曝險移到風險 change 處理。
- **[樣本小、門檻幾乎為 0、集中在少數幣與時點]** → 強制標示樣本數與「樣本不足」；分位數的 bootstrap 區間很寬（例：safety p90 約 0.20～0.75），建議值只供參考。
- **[immutable 讀取與 App 同時啟動的競態]** → 只在 App 已關閉的狀態（`-wal`、`-shm` 皆不存在）使用，讀取前後檢查檔案狀態，變動即中止。

## Open Questions

- 結算後「多腿所在交易所 mark 價相對下跌」（15/17，方向穩定）的機制未知：除息式回吐、付費方避費、跨所基差收斂、mark 公式、demo 市場自身結算、tick 與價差、領先落後都可能。區分需要正式環境 T±60 秒的 index / mark / last / 買賣一檔、主動成交方向、未平倉量，以及非結算時段的對照視窗。
- 型態閾值 0.5 是否合適？先以資料觀察，不在本 change 調整。

## Payload 形狀（`attribution`）

十進位字串；`null` = 未知。

```json
{ "entry_slippage": "0.3", "notional_per_leg": "995.0" }
```
