## 1. 純邏輯

- [x] 1.1 新增 `app/src/ui/vm/zoom.rs`：`ZoomPct` 步進 / 上下限 / 還原 / JSON 解析（壞值退回 100%），先寫失敗測試 `zoom_tests.rs`
- [x] 1.2 `theme.rs` 新增 `fs(n)`（px 值 → rems）與版面寬度常數的 rem 版本，加測試確認 100% 時換算等於原 px

## 2. 介面轉換

- [x] 2.1 `pages.rs`、`shell.rs`、`trading_pages.rs`、`font_check.rs` 的 `text_size(px(..))` 與固定寬度改用 rem helper
- [x] 2.2 100% 截圖前後比對（掃幣、交易單、手動下單頁），確認無版面位移

## 3. 快捷鍵與持久化

- [x] 3.1 `main.rs` 註冊 `ZoomIn/ZoomOut/ZoomReset` actions 與 cmd/ctrl keybindings，處理器以 `Theme::update` 設 `font_size`
- [x] 3.2 data source 埠新增 `load_ui_prefs` / `save_ui_prefs`，live 走 `config` 表 `ui_prefs`，附記憶體實作測試（讀壞值、寫入失敗只記警告）
- [x] 3.3 啟動時讀取並套用縮放；狀態列在非 100% 時顯示「縮放 N%」

## 4. 驗證

- [x] 4.1 `cargo test -p tong-funding ui::zoom` 與全套 `cargo test` 綠燈
- [x] 4.2 實機：Cmd/Ctrl +/−/0 在 70%、100%、200% 截圖；重啟後保留 130%（使用者實機確認 2026-10-06）
