## ADDED Requirements

### Requirement: 預設寬度下主要操作可見

在約 1,470 邏輯像素寬（含視窗邊框；MacBook 2560×1664 Retina 的預設視窗）、字級 100% 的視窗中，掃幣表（約 1,220 邏輯像素寬）SHALL 在 OKX 未勾選時不需橫向捲動即可看到「達標」與「加入交易單」兩欄。不在 `allowed_exchanges` 的交易所欄 SHALL 預設隱藏，使用者以欄位按鈕手動顯示的選擇 SHALL 被保留。欄位順序 SHALL 不變。

#### Scenario: OKX 停用時

- **WHEN** OKX 未勾選、視窗約 1,470 邏輯像素寬、字級 100%
- **THEN** OKX 欄預設隱藏，「達標」與「加入交易單」在可視範圍內

### Requirement: 掃幣表不截斷

Gross 欄 SHALL 完整顯示數值、「週期不同」標示與型態標籤；交易所 rate 儲存格 SHALL 完整顯示 rate、週期標籤與 8h 等效值（可分兩行）。任何文字 SHALL NOT 與相鄰欄重疊。

#### Scenario: 兩個標籤同時出現

- **WHEN** 一列同時有「週期不同」與「週期錯位」
- **THEN** 兩個標籤都完整顯示，不與 Net Edge 欄重疊

### Requirement: 資料來源狀態列清楚

「只顯示達標」開關與資料來源狀態之間 SHALL 有明確間隔；每個來源 SHALL 只顯示一次狀態與距今秒數；已停用的交易所 SHALL 顯示弱化的「已停用」，不顯示紅色 OFFLINE。

#### Scenario: OKX 停用

- **WHEN** OKX 未勾選
- **THEN** 狀態列顯示「OKX 已停用」弱化文字
