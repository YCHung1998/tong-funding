# design-tokens Specification

## Purpose
TBD - created by archiving change bootstrap-gpui-shell. Update Purpose after archive.
## Requirements
### Requirement: 色票集中定義於單一 theme 模組

所有色彩 SHALL 定義在 `app` 的單一 theme 模組中，來源為 Figma 檔 `figma-frontend_preview.fig` 實際使用的色值。
UI 程式碼 SHALL NOT 在 theme 模組之外出現十六進位色碼字面值。

背景層級 SHALL 為 `#0B1016`（最深）、`#0C131C`、`#111A24`、`#17222E`（卡片）；
分隔與邊框 SHALL 為 `#273443`；文字 SHALL 分三階：主要 `#E5EDF5`、次要 `#91A2B4`、弱化 `#7A8CA2`（Figma 為 `#586B80`，對比度不足 4.5:1，刻意提亮）；每一階文字在所有背景層級上的對比度 SHALL 不低於 4.5:1；
強調色 SHALL 為 `#58D3C5`；正向 `#65D8A2`、警示 `#E9BE67`、負向 `#F18B91`、資訊 `#789BDE`。

#### Scenario: theme 模組之外沒有色碼字面值

- **WHEN** 掃描 `app` 內 theme 模組以外的所有原始碼
- **THEN** 找不到 `#RRGGBB` 形式或等價的 RGB 常數字面值

#### Scenario: 色值與 Figma 一致

- **WHEN** 比對 theme 模組的色票與 Figma 抽出的色值清單
- **THEN** 上述每一個語意色的十六進位值完全相同，唯一的例外是弱化文字色（見上方說明）

### Requirement: 字型與字級

介面 SHALL 使用 `IBM Plex Mono` 呈現數字與英文，中文 SHALL 由 macOS 系統中文字型呈現。
`IBM Plex Mono`（Regular、Medium、SemiBold）SHALL 隨程式打包，不得依賴使用者系統已安裝。
預設內文字級 SHALL 為 11，並 SHALL 提供 9、10、12、13、14 的階層，以及大型數字用的 21、25、28。

（Figma 指定中文為 Noto Sans TC，但實測打包的 Noto 作為後備字型完全沒被 GPUI 用到，見 design.md「字型實測」；使用者決定字型只需「可顯示」，v1 不追求與 Figma 字體一致。）

#### Scenario: 中英混排能正確顯示

- **WHEN** 畫面顯示「持倉 Unified Positions 60,200.00 USDT」
- **THEN** 英文與數字以打包的 IBM Plex Mono 繪製、中文以系統字型繪製，沒有缺字方塊

#### Scenario: 沒有安裝字型的機器也能顯示

- **WHEN** 在未安裝 IBM Plex Mono 的 macOS 使用者帳號啟動程式
- **THEN** 英文與數字仍使用打包的 Plex Mono 正確顯示

### Requirement: Funding rate 與趨勢的顏色語意

系統 SHALL 提供單一語意函式決定 funding rate 的顏色：大於 0 為正向色、小於 0 為負向色、等於 0 為弱化文字色。
趨勢箭頭 SHALL 依「本次與上次比較」決定：上升為向上箭頭與正向色、下降為向下箭頭與負向色、相同則不顯示箭頭也不上色。

#### Scenario: 三種 rate 的顏色

- **WHEN** 對 rate 為 +0.01、-0.005、0 的值分別查詢顏色
- **THEN** 依序得到正向色、負向色、弱化文字色

#### Scenario: 上升趨勢

- **WHEN** 本次 rate 為 0.02、上次為 0.01
- **THEN** 回傳向上箭頭與正向色

#### Scenario: 持平不顯示箭頭

- **WHEN** 本次 rate 與上次相同
- **THEN** 不回傳箭頭，也不上色

