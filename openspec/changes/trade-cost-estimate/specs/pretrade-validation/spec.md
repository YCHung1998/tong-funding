## ADDED Requirements

### Requirement: 所需保證金計入開倉手續費

`Margin` 檢查所用的每腿「所需保證金」SHALL 為 `兩腿共同數量 × 該腿送單前最新價 ÷ 配對槓桿 + 該腿開倉手續費`，開倉手續費 = 上述價值 × 該腿 `taker_fee_pct` ÷ 100。
計算 SHALL 只使用送單前檢查已取得的資料，SHALL NOT 為此增加任何請求。
無法計算（例如缺少費率或數量）時 `Margin` SHALL 失敗。

#### Scenario: 手續費使保證金不足

- **WHEN** 數量 0.016、最新價 60,000、槓桿 5、taker 0.05，該腿可用保證金為 192.2
- **THEN** 所需保證金為 192.48（192 + 0.48），`Margin` 失敗
