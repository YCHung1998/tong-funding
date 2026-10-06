## ADDED Requirements

### Requirement: 風控方向標記色

theme 模組 SHALL 定義 `STRICT_HIGH`（越高越嚴，綠色系）與 `STRICT_LOW`（越低越嚴，紅色系）兩個方框色；兩者 SHALL 可互相區分，標記文字在其上的對比度 SHALL 不低於 4.5:1。

#### Scenario: 對比度

- **WHEN** 計算標記文字在 `STRICT_HIGH`、`STRICT_LOW` 上的對比度
- **THEN** 皆不低於 4.5:1
