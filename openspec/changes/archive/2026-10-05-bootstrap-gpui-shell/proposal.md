## Why

`tong-funding` 目前只有 `Cargo.toml`（依賴 `gpui-kit 0.7.1`）和一支 hello world 的 `main.rs`。
後續 8 個 change 都建立在「GPUI 能撐起 Figma 那 8 個畫面」這個**尚未驗證的假設**上
（528 列即時表格、甜甜圈圖、中英混排字型）。在投入 core / store / engine 之前，
先用最小的殼把這個假設驗掉，並把工作區定型成 `core` + `app` 兩個 crate。

## What Changes

- 把根目錄的單一 crate 改成 Cargo workspace：`core`（純邏輯、不依賴 GPUI）與 `app`（GPUI 桌面程式）。
- `app` 提供主視窗殼：標題列（UTC / Taipei 雙時鐘）、側邊欄 8 頁導覽、狀態列（模式徽章、連線燈、kill switch 位置）。8 頁內容皆為空白佔位。
- 從 Figma 抽出的色票、字型、字級落成單一 theme 模組。
- 兩個可行性 spike：528 列 DataTable 在可調更新頻率下的幀時間量測；甜甜圈圖（pie chart）能否正確繪製。
- 量測結果寫回本 change 的 `design.md`；未達預算時須記錄緩解決策才可封存。
- **本 change 不含任何網路、資料庫、下單程式碼**（這是本 change 的範圍界線，由 tasks 1.4 驗收，不寫成永久 spec，因為後續 change 會加入這些能力）。

## Capabilities

### New Capabilities
- `app-shell`: 主視窗、側邊欄導覽、標題列雙時鐘、狀態列、預設安全狀態、效能基準驗證。
- `design-tokens`: 色票、字型、字級、funding 正負色語意，集中於單一模組。

### Modified Capabilities
<!-- 無 -->

## Impact

- `Cargo.toml` 改為 workspace 根；`src/main.rs` 移至 `app/src/main.rs`；新增 `core/`。
- `Cargo.lock` 重新產生（版本鎖定 `gpui-kit = "=0.7.1"`）。
- 新增字型資產目錄（IBM Plex Mono、Noto Sans TC；授權於打包時確認）。
- 僅支援 macOS（後續 `store-sqlite` 使用 Keychain）。
