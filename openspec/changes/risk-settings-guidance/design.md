## Context

- 欄位清單：`app/src/ui/vm/risk_settings.rs` 的 `Field`（`GLOBAL_FIELDS` 14 個、`OVERRIDE_FIELDS` 9 個），已有 `key()`、`label()`、`unit()`。
- 方向的權威來源：`core/src/risk.rs::effective_for_pair`。取 max：`min_24h_volume_usdt`、`safety_margin_pct`、`net_edge_threshold_pct`、`est_slippage_pct`；取 min：`max_leverage`、`max_price_drift_pct`、`stale_data_threshold_ms`、`order_timeout_seconds`、`max_leg_imbalance_pct`。全域限定：`max_concurrent_pairs`（越低越嚴）、`min_expected_net_pnl_pct`（越高越嚴）、`taker_fee_pct`（不參與覆寫合併）。
- 頁面：`trading_pages.rs::risk_settings_page` 以 `field_row(label, unit, input, err)` 逐列渲染；標題用 `title("風控設定", "Risk Management")`。
- Dialog：gpui-component 0.7.1 `WindowExt::open_dialog(cx, |dialog, window, cx| ...)`（`window_ext.rs:31`），dialog layer 由 Root plugin 繪製（`root.rs:491`），`gpui_kit::open_window` 已包 Root。

## Goals / Non-Goals

**Goals:** 一眼看出每個欄位往哪個方向調是收緊；集中說明每個參數的公式與作用。

**Non-Goals:** 改任何預設值、驗證或合併規則；在其他頁面加說明；多語系。

## Decisions

1. **`Field::strictness() -> Strictness { HigherStricter, LowerStricter, Fact }`** 純函式；一致性測試直接呼叫 core 的 `effective_for_pair`（每個覆寫欄位：兩腿覆寫 1 與 2，看結果取哪個），避免方向表與合併規則各寫一份而漂移。
2. **`Field::help() -> FieldHelp { meaning, formula: Option<&str>, affects: &[Affect] }`** 純資料，說明視窗與測試共用；文字以 spec 的公式為準。
3. **顏色**：新增 `STRICT_HIGH`、`STRICT_LOW` 兩個方框底色（綠 / 紅系，暗色），標記文字用 `TEXT_PRIMARY`；不重用 `POSITIVE`/`NEGATIVE` 文字色，避免與 funding 正負、盈虧的語意混淆。方向永遠附箭頭文字。
4. **`?` 按鈕 → `window.open_dialog`**：內容為可捲動清單（欄位多），寬度用 `rx(..)` 以跟隨縮放。
5. **字級**：只用既有 helper（`small`、`text`）與 `units::fs/rx`，不新增 px。

## Risks / Trade-offs

- [`open_dialog` 在本 app 從未用過，dialog layer 實際是否繪製未驗證] → 先做最小 spike；不行則改為頁內可收合說明區塊（同一份 `help()` 資料）。
- [`est_slippage_pct`「越高越嚴」違反直覺（它是估計值）] → 說明文字明寫：估得越高，Net Edge 算得越低、越不容易達標，因此較保守。

## 實作紀錄

- **`open_dialog` spike 成功**：`window.open_dialog(cx, |dialog, window, _| ...)` 在本 app 的 Root 下實際繪製，內容可捲動；以點擊外側遮罩或 `close_dialog` 關閉。寬度 API 需要 `Pixels`，以 `rx(640.0).to_pixels(window.rem_size())` 取得，仍跟隨縮放。不需退回頁內可收合區塊。測試：`app/src/ui/risk_help_ui_tests.rs`。
- 預設值改 3000 後，原本隱含 1000 的兩個測試（`node0` 過期邊界、合約試算過期報價）改用 3000 / 3001 ms；`core/tests/parity_fixtures.rs` 的 `1_000_000` 為 Python 來源資料，未動。
