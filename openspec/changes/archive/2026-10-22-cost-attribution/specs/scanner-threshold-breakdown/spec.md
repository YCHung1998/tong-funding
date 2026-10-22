## ADDED Requirements

### Requirement: 掃幣列顯示機會型態

掃幣頁每個候選列 SHALL 顯示該機會的型態標籤（`週期錯位`、`費率差`；型態為 `None` 時不顯示標籤），並在標籤的說明文字中列出每小時費率差。
標籤 SHALL 只做顯示，SHALL NOT 影響「只顯示達標」篩選、排序預設或加入交易單的行為。

#### Scenario: 週期錯位標籤

- **WHEN** 候選為 Bybit 8h 多、Binance 4h 空，型態為 `IntervalMismatch`
- **THEN** 該列顯示「週期錯位」，說明文字包含每小時費率差

#### Scenario: 不影響達標

- **WHEN** 兩個型態不同但 Net Edge 都達門檻的候選
- **THEN** 兩者都出現在「只顯示達標」，順序仍由原排序規則決定
