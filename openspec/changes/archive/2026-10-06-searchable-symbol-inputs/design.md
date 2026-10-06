## Context

- 目前 4 個標的類輸入框都是 `Entity<InputState>`（`trading_pages.rs` `c_symbol`、`m_symbol`、`x_symbol`、`r_coins`），值以每次 render 輪詢 `val(input)` 讀取；app 從未訂閱輸入事件。
- gpui-component 0.7.1 有 `SelectState::new(SearchableVec::new(items), selected, window, cx).searchable(true)`（單選，事件 `SelectEvent::Confirm`）與 `ComboboxState::new(..).multiple(true).searchable(true)`（多選，事件 `ComboboxEvent::Change/Confirm`），`set_items` 可更新候選、`selected_value()` 讀值。預設 `matches` 為不分大小寫子字串。
- 候選資料：`snap.market[ex].observations[].symbol`；`format::base_coin` 取基礎幣；掛單在 `leg_accounts[..].open_orders`。

## Goals / Non-Goals

**Goals:** 4 個欄位有可搜尋、可捲動的候選；允許自由輸入。

**Non-Goals:** 模糊比對（拼字容錯）；最近使用排序；數值欄位（數量、槓桿等）。

## Decisions

1. **單選用 `Select`(searchable)、多選用 `Combobox`(multiple + searchable)**：沿用元件內建搜尋框與捲動，不自寫 popover。
2. **自由輸入**：`Select` 不支援輸入任意值，故以 `SearchableVec` 的候選加上「目前搜尋字串（大寫、trim）作為第一個候選項」實作：當搜尋字串不在清單中時，清單首列為「使用 `NEWUSDT`」。由純函式 `options_with_query(query, options)` 產生並測試。
   - 替代方案：保留 Input + 自製下拉 → 要自己處理焦點、捲動與鍵盤，工作量大且易錯；捨棄。
3. **候選更新**：快照重算時（`recompute`）若候選集合改變才 `set_items`，避免每 100ms 重建清單、打斷使用者操作；保留目前選取值。
4. **讀值仍走輪詢**：`manual_form` / `contract_form` / `risk_form` 改讀 `selected_value()`，保持既有「每次 render 比對」的模式，不引入事件訂閱。

## Risks / Trade-offs

- [行情有數百個 Symbol，清單渲染效能] → `Select` 的清單是虛擬化的 list；實機以全市場資料確認輸入無延遲。
- [Decision 2 的「搜尋字串即候選」需依賴 `perform_search` 時能拿到 query] → 實作前先以小 spike 確認 `SearchableVec` 能否在搜尋時插入動態項；不行則改為自訂 `SearchableListDelegate`（同一純函式）。
- [與 `manual-order-position-picker` 的帶入寫值衝突] → 帶入改為 `set_selected_value`；兩 change 依序實作。
