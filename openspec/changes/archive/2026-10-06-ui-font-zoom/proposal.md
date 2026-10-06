## Why

介面字級固定為 11px（多處 `px(10.0)` 寫死），在大螢幕或久看時偏小，使用者無法自行調整。需要像瀏覽器一樣以 Ctrl/Cmd + / − 縮放整個介面，並記住偏好。

## What Changes

- 新增全域快捷鍵：`Cmd/Ctrl` + `=`（或 `+`）放大、`Cmd/Ctrl` + `-` 縮小、`Cmd/Ctrl` + `0` 還原 100%。
- 縮放範圍 70%–200%，每步 10%；文字、間距、側邊欄寬、輸入框寬一起等比縮放。
- 縮放比例存於 SQLite `config` 表（key `ui_prefs`），下次啟動還原；讀取失敗或值不合法時退回 100%。
- 狀態列顯示目前縮放比例（非 100% 時）。
- 縮放透過 gpui-component 的 `Theme.font_size` 實現（其 Root 每幀以此設定 rem），因此 app 內寫死的 `px` 字級與寬度改為 rem 基準；100% 時畫面與現在相同。

## Capabilities

### New Capabilities
- `ui-zoom`: 介面縮放快捷鍵、縮放範圍與步進、偏好持久化與還原。

### Modified Capabilities
（無。`design-tokens` 的字級階層仍以 100% 為基準定義，縮放是乘上倍率。）

## Impact

- `app/src/ui/theme.rs`、`pages.rs`、`shell.rs`、`trading_pages.rs`、`font_check.rs`：`text_size(px(N))` 與版面寬度改為 rem 基準。
- `app/src/ui/component_theme.rs`：改用 `Theme::update` 設定 `font_size`。
- `app/src/main.rs`：註冊 actions 與 keybindings。
- 新純函式模組 `app/src/ui/vm/zoom.rs`（步進、上下限、序列化），附測試。
- `app/src/store`：`ui_prefs` 讀寫（沿用 `config_get` / `config_set`）。
