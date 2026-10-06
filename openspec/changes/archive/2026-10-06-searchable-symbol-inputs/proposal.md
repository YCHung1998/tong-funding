## Why

Symbol / 幣種輸入框目前都是純文字框，使用者要完整記得並正確拼出 `1000PEPEUSDT` 之類的名稱，打錯只會在之後才發現（規則查不到、風控白名單無效）。應在輸入到一半時就提供可捲動的候選清單直接選取。

## What Changes

- 以下輸入框改為「可搜尋下拉」（gpui-component `Select` / `Combobox` + `SearchableVec`，輸入即過濾、可捲動、鍵盤上下選取）：
  - 合約設定頁「試算標的」（Symbol，單選）
  - 手動下單頁 Symbol（單選，候選依目前選取的交易所）
  - 撤單 Symbol（單選，候選為該所有掛單的 Symbol；Order ID 由 `manual-order-position-picker` 處理）
  - 風控設定 `allowed_coins`（多選幣種）
- 候選來源為最新行情快照中的 Symbol（依交易所）或其基礎幣（`format::base_coin`），排序固定（字母序）。
- 仍允許輸入不在清單中的值（例如剛上市、尚未出現在快照），並在旁邊提示「不在目前行情清單中」。

## Capabilities

### New Capabilities
- `symbol-search-input`: 標的 / 幣種輸入框的候選來源、過濾規則與自由輸入行為。

### Modified Capabilities
（無。）

## Impact

- `app/src/ui/vm/` 新增純函式 `symbol_options(snap, exchange)`、`coin_options(snap)`、`filter_options(query, options)`，附測試。
- `app/src/ui/trading_pages.rs`：`c_symbol`、`m_symbol`、`x_symbol`、`r_coins` 由 `InputState` 換成 `SelectState` / `ComboboxState`；讀值處（`manual_form`、`contract_form`、`risk_form`）改讀選取值。
- 依賴：gpui-component 0.7.1 既有元件，不新增 crate。
- 建議在 `manual-order-position-picker` 之後實作（兩者都改手動下單頁）。
