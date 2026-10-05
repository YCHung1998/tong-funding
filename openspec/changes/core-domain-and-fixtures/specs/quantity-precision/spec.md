## ADDED Requirements

### Requirement: 下單數量只能向下取整到交易所步長

系統 SHALL 以各交易所的 `step_size` 將數量向下取整，SHALL NOT 向上取整。
取整後若小於 `min_qty`，SHALL 回傳「低於最小下單量」錯誤，呼叫端 SHALL NOT 以 0 或原數量送單。
所有計算 SHALL 使用 Decimal，不得依賴浮點誤差容忍值（例如 epsilon）。

#### Scenario: 向下取整

- **WHEN** 數量為 2.14589213、`step_size` 為 0.1、`min_qty` 為 0.1
- **THEN** 結果為 2.1

#### Scenario: 剛好在步長倍數上不會被誤減一格

- **WHEN** 數量為 2.3、`step_size` 為 0.1
- **THEN** 結果為 2.3，而不是 2.2

#### Scenario: 低於最小下單量

- **WHEN** 數量為 0.0004、`step_size` 為 0.001、`min_qty` 為 0.001
- **THEN** 回傳「低於最小下單量」錯誤

### Requirement: 送出的數量字串的小數位數由步長決定

系統 SHALL 產生送往交易所的數量字串，小數位數 SHALL 由 `step_size` 推導，SHALL NOT 使用固定位數。

#### Scenario: 依步長決定位數

- **WHEN** `step_size` 為 0.1，數量為 2.1
- **THEN** 字串為 `2.1`
- **AND** 當 `step_size` 為 1、數量為 2 時，字串為 `2`

#### Scenario: 較細的步長

- **WHEN** 名目本金 1,200 USDT、價格 60,200、`step_size` 為 0.001
- **THEN** 取整後的數量字串為 `0.019`

### Requirement: OKX 數量須先換算為張數再取整

OKX 以合約張數下單。系統 SHALL 先以 `ct_val` 將幣量換算為張數（幣量 ÷ `ct_val`），再套用 `lotSz` 與 `minSz` 取整。
`ct_val` 小於或等於 0 時 SHALL 回傳錯誤。

#### Scenario: 幣量換算為張數

- **WHEN** 目標幣量為 0.021 BTC、`ct_val` 為 0.01、`lotSz` 為 1、`minSz` 為 1
- **THEN** 張數為 2（對應 0.02 BTC），而不是 0.021 張

### Requirement: 平倉數量取自交易所回報的實際持倉

系統 SHALL 提供「由交易所持倉建構數量」的方式，其值取持倉數量的絕對值，且 SHALL NOT 再套用步長取整。
持倉為 0 時 SHALL 回傳「無持倉」錯誤。一般開倉數量 SHALL 只能經由取整函式建構。

#### Scenario: 以實際持倉平倉

- **WHEN** 交易所回報持倉為 −0.019（空單）
- **THEN** 平倉數量為 0.019，不經過取整

#### Scenario: 無持倉不可平倉

- **WHEN** 交易所回報持倉為 0
- **THEN** 回傳「無持倉」錯誤
