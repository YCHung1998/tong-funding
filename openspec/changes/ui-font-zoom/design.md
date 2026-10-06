## Context

- 文字字級全部以 `px(...)` 指定（`pages.rs` 的 `small()` = `px(10)`、`title()` = `px(21)`、shell 根節點 `px(FONT_SIZE_BODY)` 等，約 16 處 `text_size(px(` 加上 helper）。GPUI 的 `px` 不隨 rem 縮放，`rems` 才會。
- `gpui_kit::open_window` 以 gpui-component 的 `Root` 包住視圖，其 `WindowState` plugin 每幀執行 `window.set_rem_size(cx.theme().font_size)`（gpui-component `root.rs:436`）。因此自行呼叫 `window.set_rem_size` 會被覆蓋，唯一槓桿是 `Theme.font_size`（預設 16px）。
- gpui-component 的 Input、DataTable 文字與 spacing helper（`p_3`、`gap_2` …）本來就是 rem；表格列高是固定 px（26–40px）。
- 目前 app 沒有任何 actions / keybindings / menus。
- `config` 表（`state.rs` `config_get`/`config_set`）已存 `risk`、`risk_overrides`，可加一個 key。

## Goals / Non-Goals

**Goals:** 一個縮放倍率控制全部文字與版面；快捷鍵；持久化。

**Non-Goals:** 個別頁面各自縮放；調整 gpui-component 表格的固定列高（200% 時字仍放得進 26px 列高的最小值，見風險）；選單列（menu bar）項目。

## Decisions

1. **以 rem 為單位、改 `Theme.font_size`**：把 app 內所有 `text_size(px(N))` 改為 `rems(N / 16.0)`（集中在 `theme.rs` 新增 `fn fs(n: f32) -> Rems`），固定寬度（側邊欄 200、header 44、輸入框 160/110、掃幣表高 560）同樣改 rems。縮放時只需 `Theme::update(cx, |t| t.font_size = px(16.0 * zoom))`。
   - 替代方案：全域倍率乘在每個 `px` 上 → 每處都要傳倍率，侵入性高且容易漏；捨棄。
   - 100% 時 `16 × N/16 = N`，畫面與現在逐像素相同（以截圖比對驗證）。
2. **純函式 `ui::zoom`**：`ZoomPct(u16)`，`step_in/step_out/reset/clamp`、`from_json/to_json`，無 GPUI 型別，可單元測試。
3. **Actions**：`actions!(tong, [ZoomIn, ZoomOut, ZoomReset])`，`cx.bind_keys` 綁 `cmd-=`、`cmd-+`、`cmd--`、`cmd-0` 以及對應的 `ctrl-` 版本（使用者明確要求 Ctrl）；`cx.on_action` 全域處理，更新 theme 並 `cx.refresh_windows()`。
4. **持久化**：Shell 透過既有的 data source 埠新增 `load_ui_prefs()` / `save_ui_prefs()`（live 實作走 `config_get/config_set`，測試用記憶體實作）。每次縮放立即寫入；樂觀版本衝突時以「讀最新版 → 覆寫」重試一次。
5. **`component_theme` 改用 `Theme::update`**：避免 `global_mut` 改到的欄位沒同步到 `tokens`（這也是 scanner-readability 的根因之一）；本 change 只處理 `font_size`，顏色同步留給 `scanner-readability`。

## Risks / Trade-offs

- [200% 時 DataTable 固定列高裁切文字] → 掃幣表格內容 `small()` 在 200% 為 20px，列高最小 26px 仍放得下；實機截圖確認。
- [轉換 px→rems 漏改或改錯導致 100% 時版面位移] → 轉換前後各截一張 100% 截圖比對；`font_check` 測試保留。
- [Cmd+- 與輸入框內的系統快捷鍵衝突] → Input 元件不使用這些組合；實機確認輸入框聚焦時快捷鍵仍有效。
