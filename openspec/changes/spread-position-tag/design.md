## Context

- 持倉頁的配對卡（`app/src/ui/vm/positions.rs` 的 `PairCard`）與交易單頁的進行中列（`app/src/ui/vm/staged_orders.rs` 的 `RunningRow`）都是從 `UiSnapshot` 純函式建出的 view model。
- 各所最新價格在 `UiSnapshot.market[exchange].observations`（`FundingObservation.mark_price`）：REST 每 10 秒更新，Binance 與 Bybit 另有 WebSocket 每秒覆蓋（`MarketPartial`），資料來源失敗與過舊由 `MarketFeed::failing()` 與觀測時間判斷（`candidates::eligibility` 已用同一套規則）。
- 價格是標記價，不是買賣盤報價；兩所標記價的定義不同，價差只是方向性參考。

## Goals / Non-Goals

**Goals:**
- 持倉期間即時顯示 positive／negative／neutral 與價差百分比。
- 純顯示、純函式、可測試；缺資料不猜。

**Non-Goals:**
- 不用在達標判定、進場、平倉或提早平倉。
- 不寫入資料庫或事件、不新增設定。
- 不顯示開倉當時的固定標籤（使用者選擇持續更新的即時版本）。
- 不使用買賣盤報價。

## Decisions

1. **純函式放在 `app/src/ui/vm/spread_tag.rs`**：`spread_tag(long_price: Option<Price>, short_price: Option<Price>) -> SpreadTag { kind, pct }`，`kind ∈ {Positive, Negative, Neutral, Unknown}`。與 vm 其他模組一致，不依賴 GPUI。
2. **價格取得與新鮮度判斷放在 vm 層**：`positions::build` 與 `staged_orders::build` 呼叫一個共用的小函式，依 `(long_exchange, short_exchange, symbol)` 從 `snap.market` 找觀測，來源 `failing()` 或觀測 `observed_at` 超過 `stale_data_threshold_ms` 就回傳 `None`，由純函式變成 `Unknown`。理由：與 `candidates::eligibility` 的新鮮度規則一致。
3. **價差百分比以 long 價格為分母**：`(short − long) ÷ long × 100`；long 價格為 0 時回傳 `Unknown`，避免除以零。
4. **顯示用文字加色調**：`positive`／`negative`／`neutral` 文字一律顯示，色調用既有的 `Tone::Positive`／`Negative`／`Muted`。
5. **只對仍持有部位的狀態顯示**：`RECONCILED`、`CLOSING`（未確認空倉）、`PARTIAL_FAILURE`、`IMBALANCED`、`UNRESOLVED` 且至少一腿仍有部位時顯示（沿用 `candidates::staged` 的「仍持倉」判斷）；其他狀態不顯示。

## Risks / Trade-offs

- [標記價不是可成交價，兩所標記價的計算方式不同，標籤可能與實際可成交價差方向不同] → 只當作方向參考，文件與畫面註明「標記價」；不影響任何決策。
- [REST 價格每 10 秒更新，WebSocket 失效時標籤會變慢] → 資料過舊即不顯示標籤，不會顯示過期的方向。
- [`positive`／`negative` 的方向解讀（價差收斂有利或不利）是使用者的理論假設] → 以 8 筆實單的價差損益與標籤對照驗證（tasks 4.2），結果記錄在 `issue/`，不在程式中假設因果。

## Migration Plan

純顯示、無資料與設定變更；直接部署，回滾即還原 commit。

## Open Questions

- 標籤是否也要出現在掃幣頁（進場前以兩腿現價預判）？本提案只做持倉期間；若需要另案。
- 價差百分比是否要顯示在表格欄位，還是只在配對卡內顯示？（預設兩處都顯示。）
