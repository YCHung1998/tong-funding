## Context

- 手動下單送出的是 `Command::ManualOrder { exchange, symbol, side, quantity, reduce_only }`；頁面沒有「平倉」概念，平倉 = 反方向 + reduce_only。
- `UiSnapshot.leg_accounts: BTreeMap<(simulated, Exchange), LegAccount>`，`positions: Result<Listed<AccountPosition>, String>`，`AccountPosition { exchange, symbol, quantity }`（多為正、空為負），`open_orders` 為 `Listed<AccountOrder>`（含 `client_order_id: Option<String>`）。由 `live.rs` 每 `LEG_ACCOUNT_POLL_MS` 輪詢模擬與 demo 兩種帳戶。這是 engine 實際下單的帳戶視角，比 `accounts`（交易所真實帳戶）更符合手動下單的目標環境。
- 輸入框是 `InputState`，以 `set_val(input, value, window, cx)` 寫入；交易所、方向、reduce_only 是 `TradingState` 欄位。

## Goals / Non-Goals

**Goals:** 一鍵帶入平倉 / 撤單參數，杜絕手打方向、數量、id 的錯誤。

**Non-Goals:** 一鍵直接送出（仍要確認）；配對（pair）層級的平倉（已在交易單頁 `ManualExit` / `ManualClose`）；部分平倉比例按鈕。

## Decisions

1. **資料來源用 `leg_accounts[(mode == SIMULATION, ex)]`**：與 engine 的執行器一致，SIMULATION 下平的是模擬持倉。
2. **純函式在 VM**：`open_positions(snap) -> PositionsList`（每所 `Rows | Failed(msg) | Incomplete(rows)`）、`close_prefill(&AccountPosition) -> ManualPrefill { exchange, symbol, side, quantity, reduce_only: true }`、`open_orders(snap)`、`cancel_prefill`。全部可單元測試，GPUI 層只負責渲染與把 prefill 寫進輸入框。
3. **數量以字串原樣帶入**（`quantity.abs().normalize()`），不先取整；取整仍由既有 `build` 依交易所步長處理，確認視窗會顯示取整結果。
4. **帶入不另外請求規則**：下一次 render 時既有邏輯（比對 `m_requested`）偵測到 Symbol 變更，會自動 `request_rules`，確保步長已載入。

## Risks / Trade-offs

- [持倉資料最多延遲 15 秒，帶入的數量可能已過時] → reduce_only 保證不會反向加倉；清單標示「更新於 N 秒前」。
- [quantity 正負號慣例若在某交易所相反] → 以 `AccountPosition` 文件定義（多正空負）為準，測試涵蓋兩方向。
