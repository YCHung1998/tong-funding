## Context

- 結算時間 `T` 目前定義為兩腿 `next_funding_time` 的較小值（`core/src/funding.rs` 的 `pair_settlement`），只有時間等於 `T` 的腿計入資金費（`core/src/net_edge.rs` 的 `compute_net_edge`）。規格把「週期不同時只計入會結算的腿」寫成合法情境。
- 「達標」只看淨邊際、上架狀態、成交量、交易所與幣種允許清單（`evaluate_oriented`），沒有任何一條要求兩腿結算時間相同。
- 送單前檢查有**另一份**判斷：`app/src/engine/node0.rs` 的 `NetEdgeQualified` 直接呼叫 `compute_net_edge` 並自己比較門檻與最低預期淨利，**不經過** `evaluate_oriented`。所以只改 core 的達標條件，掃幣會擋、進場前檢查不會。
- 掃幣列的 `Qualified::Yes` 才能被勾選與自動加入（`app/src/ui/vm/candidates.rs` 的 `eligibility`），所以擋掉達標就同時擋掉「進入交易單」。
- 2026-10-14／15 八筆實單中，3 筆（RSR 02:00、RSR 04:00、BLUR 04:00）進場快照裡兩腿下次結算時間不同，是被現行規則放行的。

## Goals / Non-Goals

**Goals:**
- 兩腿下次結算時間不同（含任一腿缺失）的配對不達標：不可勾選、不會被自動加入、送單前檢查 BLOCK。
- 掃幣與送單前檢查使用同一條規則，不再各寫一份。
- 掃幣頁看得出「沒達標是因為結算時間不同」。

**Non-Goals:**
- 不改 Net Edge 的計算（仍是單次結算模型，單邊結算時只計入會結算的腿，供顯示）。
- 不改進出場時間（T−10 秒進、T+15 秒出）、手續費與滑價設定。
- 不處理 Binance demo 與正式環境結算週期不同的問題（001）；不處理資金費流水延遲（003）。
- 不處理「進場後只剩單腿成交」的異常流程（`PARTIAL_FAILURE` 等既有機制不變）。

## Decisions

1. **同步條件放在 core 的達標判定，並抽成一個小函式。** 新增 `legs_settle_together(long, short) -> bool`（兩腿 `next_funding_time` 相等且大於 0），`evaluate_oriented` 的 `qualifies` 加上它。理由：兩個方向的結果必然相同，不影響 `better()` 的選擇；掃幣、自動加入都共用同一個入口。替代方案：只在掃幣頁過濾列——被否決，因為送單前檢查與自動加入仍可能放行。
2. **送單前檢查 `NetEdgeQualified` 呼叫同一個函式。** `node0.rs` 在判定 `net_edge_qualified` 時加上 `legs_settle_together(&long.pretrade.funding, &short.pretrade.funding)`，不同步時加一條 `notes` 說明原因（兩腿下次結算時間），整體仍歸在 `NetEdgeQualified` 底下（不新增 Check 項目，維持封閉清單與既有 UI 對應）。替代方案：新增第 11 個 `Check::SettlementSynced`——被否決，會改封閉列舉、事件與 UI 的所有對應，且本質上是「達標」的一部分。
3. **Net Edge 的數值照算。** 不同步的配對 `compute_net_edge` 結果不變（只計會結算的腿），不達標只由 `qualifies` 決定。理由：掃幣頁要能顯示這個數字、既有的單次結算規格與 parity fixtures 不必改。
4. **掃幣列的原因顯示**：`Qualified` 增加一個變體 `NotSynchronized`（顯示「結算不同步」），僅在「淨邊際與其餘條件不論，兩腿結算時間不同」時出現，`eligibility` 把它與 `No` 同樣視為不可勾選。理由：現在 `No` 只顯示「—」，使用者看不出是結算時間造成。替代方案：不加變體、只在 tooltip 說明——被否決，因為這次的問題正是「看不出為什麼會進場」。
5. **缺失視為不同步（失敗即封閉）**：`next_funding_time` 為 0 或沒有資料時不達標，與專案既有的「缺資料視為失敗」一致。
6. **既有 PREPARED 配對不另做遷移**：進場前檢查會用最新資料重判，不同步者 BLOCK；不需資料庫變更。

## Risks / Trade-offs

- [Binance demo 為 8 小時週期，掃幣用正式環境資料] → 正式環境「同步」的配對（例：兩所都是 4 小時），在 demo 可能只有 Bybit 結算。本變更不解決；列為已知限制，另案處理（需要讓掃幣改用 demo 的結算時間，或限制 demo 可交易的結算點）。
- [掃幣資料的 `next_funding_time` 在結算瞬間可能暫時不一致（一所已跳到下一期、另一所還沒）] → 結果是暫時不達標、不進場，屬於保守方向；Bybit WebSocket 的覆蓋（以較新的訊息為準）不改變此判斷。
- [BREAKING：週期不同、只有一邊結算的機會全部消失，策略機會變少] → 這是使用者明確要求；002、004 的資料顯示這類單邊結算的實際損益並不比同步結算好（RSR −0.63、BLUR 04:00 −2.99）。
- [`core` 的 parity fixtures 若含單邊結算且達標的案例會失敗] → 這些案例要改寫成「不達標」，並保留一個「計算仍正確」的斷言，不能直接刪除。

## Migration Plan

1. 先寫失敗的測試（core 達標判定、node0 送單前檢查、掃幣列），再改程式。
2. 部署即生效，無設定與資料庫變更。
3. 回滾：還原這次的 commit；沒有遷移步驟。

## Open Questions

- 「結算不同步」這個顯示字樣是否可以？（掃幣列「達標」欄位）
- 是否要在系統日誌留下事件（例如進場前檢查因結算不同步而 BLOCK 時，現有的 `BLOCKED`／`PAIR_TRANSITION` 事件已含失敗檢查名稱與 notes，預設不另加事件）？
- Binance demo 與正式環境結算週期不一致（見 Non-Goals）要怎麼處理，是否另開變更？
