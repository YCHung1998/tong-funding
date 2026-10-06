## ADDED Requirements

### Requirement: 表格斑馬紋色

theme 模組 SHALL 定義表格專用的兩個列底色：奇數列 `TABLE_ROW` = `#0B1016`（同最深背景）、偶數列 `TABLE_STRIPE` = `#182430`，兩者亮度比 SHALL 不低於 1.2（原本 `#0B1016` 對 `#0C131C` 僅 1.02，肉眼無法分辨）；另定義 `TABLE_HOVER`，與上述兩色皆可區分。三階文字在這三個底色上的對比度 SHALL 不低於 4.5:1。
套用到 gpui-component 的顏色 SHALL 經由會同步繪製 tokens 的 API（`Theme::update`）設定，確保實際繪製顏色等於 theme 模組定義的值。

#### Scenario: 對比度

- **WHEN** 計算主要、次要、弱化文字色在 `TABLE_ROW`、`TABLE_STRIPE`、`TABLE_HOVER` 上的對比度
- **THEN** 皆不低於 4.5:1

#### Scenario: 奇偶列可分辨

- **WHEN** 計算 `TABLE_ROW` 與 `TABLE_STRIPE` 的亮度比
- **THEN** 不低於 1.2

#### Scenario: 實際繪製使用設定值

- **WHEN** 套用 component theme 後讀取 gpui-component 的 `tokens.table_even`
- **THEN** 其值等於 `TABLE_STRIPE`
