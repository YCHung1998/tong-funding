## MODIFIED Requirements

### Requirement: Funding rate 與趨勢的顏色語意

系統 SHALL 提供單一語意函式決定 funding rate 的顏色：大於 0 為正向色、小於 0 為負向色、等於 0 為弱化文字色。
趨勢箭頭 SHALL 依「本次與上次比較」決定：上升為向上箭頭與正向色、下降為向下箭頭與負向色、相同則不顯示箭頭也不上色。
此處的正向色與負向色 SHALL 是兩種不帶好壞含意的 rate 專用顏色（SHALL NOT 使用表示成功與危險的綠、紅），兩者 SHALL 可互相區分，且在背景上的對比度 SHALL 不低於 4.5:1；表示成功與危險的顏色 SHALL 只用於 Net Edge 與損益。

#### Scenario: 三種 rate 的顏色

- **WHEN** 對 rate 為 +0.01、-0.005、0 的值分別查詢顏色
- **THEN** 依序得到 rate 正向色、rate 負向色、弱化文字色，且前兩者都不是成功色或危險色

#### Scenario: 上升趨勢

- **WHEN** 本次 rate 為 0.02、上次為 0.01
- **THEN** 回傳向上箭頭與 rate 正向色

#### Scenario: 持平不顯示箭頭

- **WHEN** 本次 rate 與上次相同
- **THEN** 不回傳箭頭，也不上色
