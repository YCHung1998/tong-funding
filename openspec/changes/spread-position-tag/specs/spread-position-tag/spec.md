## ADDED Requirements

### Requirement: 持倉配對的價差標籤

對每一個持有部位的配對，系統 SHALL 以 long 腿所在交易所與 short 腿所在交易所各自的**最新價格**比較，產生下列標籤：

- long 價格小於 short 價格：`positive`；
- long 價格大於 short 價格：`negative`；
- 兩者相等：`neutral`；
- 任一腿的價格缺失，或其資料來源已失敗／過舊（超過 `stale_data_threshold_ms`）：不產生標籤（`unknown`），SHALL NOT 以舊價格或 0 代替。

標籤 SHALL 同時附上價差百分比：(short 價格 − long 價格) ÷ long 價格 × 100，以 Decimal 計算且不在計算層四捨五入；`positive` 對應正值、`negative` 對應負值。標籤 SHALL 僅供顯示，SHALL NOT 影響達標判定、進場、平倉或提早平倉。

#### Scenario: 多單價格較低為 positive

- **WHEN** long 腿所在交易所價格 100、short 腿所在交易所價格 100.2
- **THEN** 標籤為 `positive`，價差百分比為 +0.2

#### Scenario: 多單價格較高為 negative

- **WHEN** long 腿所在交易所價格 100.3、short 腿所在交易所價格 100
- **THEN** 標籤為 `negative`，價差百分比為負值

#### Scenario: 價格相同為 neutral

- **WHEN** 兩腿價格相同
- **THEN** 標籤為 `neutral`，價差百分比為 0

#### Scenario: 價格缺失或過舊不產生標籤

- **WHEN** short 腿所在交易所的行情資料已失敗，或最新價格超過 `stale_data_threshold_ms`
- **THEN** 不顯示標籤，也不使用舊價格推算

### Requirement: 標籤在持倉期間持續更新並顯示於持倉頁與交易單頁

標籤 SHALL 隨最新行情持續更新（不以開倉當時的價格固定）。`RECONCILED` 的配對 SHALL 在交易單頁的進行中列表顯示標籤與價差百分比；所有仍持有部位的配對 SHALL 在持倉頁的配對卡顯示。`positive` SHALL 使用正向色調、`negative` 使用負向色調、`neutral` 與無資料使用靜音色調，並以文字（`positive`／`negative`／`neutral`）呈現，不得只以顏色區分。

#### Scenario: 行情更新後標籤跟著改變

- **WHEN** 配對持倉中，long 價格由 100 變為 100.5、short 價格維持 100.2
- **THEN** 標籤由 `positive` 變為 `negative`，不需重新整理頁面

#### Scenario: 非持倉配對不顯示

- **WHEN** 配對為 `PREPARED`、`CANCELLED` 或已 `FINALIZED`
- **THEN** 不顯示標籤
