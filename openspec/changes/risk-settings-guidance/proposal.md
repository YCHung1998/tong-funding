## Why

風控設定頁的數值欄位方向不一：有些「數字越高越嚴格」（如 Net Edge 門檻、最低成交量），有些「數字越低越嚴格」（如最大槓桿、允許價格漂移）。目前畫面不區分，使用者要調嚴或調鬆時必須自己記住每個欄位的方向，也看不到每個參數用在哪條公式。

## What Changes

- 風控設定頁每個數值欄位前加上**有色方框標記與方向文字**：
  - 「▲ 越高越嚴」（綠色系方框）
  - 「▼ 越低越嚴」（紅色系方框）
  - 「● 事實值」（中性色方框）：用於 taker 手續費，應照帳戶等級實填，不是調嚴調鬆的旋鈕
  顏色只是輔助，方向一律同時以箭頭文字表示（色盲可辨）。
- 每腿覆寫區的欄位使用相同標記。
- 「風控設定 Risk Management」標題旁新增 `?` 按鈕，點擊開啟說明視窗：逐欄列出意義、所在公式、影響的判斷（達標 / 送單前檢查 / 執行），並附 Net Edge 與「所需費率價差」公式。
- `stale_data_threshold_ms` 預設由 1000 改為 **3000**（使用者 2026-10-06 決定採用此建議值），並在風控頁該欄位後加註原因：
  「建議 3000 ms：送單前檢查用的是送單前剛抓的價格與費率，正常延遲約 0.1–0.5 秒、交易所慢時 1–2 秒；1000 容易因網路延遲誤擋，超過 5000 則送單時的價格可能已明顯偏離（另有 max_price_drift_pct 把關價格變動）。」
- 方向的唯一來源是 core 的保守合併規則（`effective_for_pair` 取 max 的欄位 = 越高越嚴，取 min 的欄位 = 越低越嚴）；以測試鎖定兩者一致。

## Capabilities

### New Capabilities
- `risk-settings-guidance`: 風控欄位的嚴格方向標記與參數說明視窗。

### Modified Capabilities
- `risk-config`: `stale_data_threshold_ms` 預設值 1000 → 3000。
- `design-tokens`: 新增方向標記用的兩個方框色 token（不改既有色值）。

## Impact

- `app/src/ui/vm/risk_settings.rs`：`Field::strictness()`、`Field::help()` 純函式與測試。
- `app/src/ui/theme.rs`：`STRICT_HIGH`、`STRICT_LOW` 色 token。
- `app/src/ui/trading_pages.rs`：欄位標記、`?` 按鈕與 dialog（gpui-component `WindowExt::open_dialog`，dialog layer 已由 gpui-kit 的 Root 繪製）。
- `core/src/risk.rs` 預設值 1000 → 3000；`risk-config` spec 的預設值表（MODIFIED）。
- 不改驗證或合併邏輯。
