# quantity-precision Specification

## Purpose
TBD - created by archiving change core-domain-and-fixtures. Update Purpose after archive.
## Requirements
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

### Requirement: 平倉數量取自交易所回報的實際持倉，且是獨立的型別

系統 SHALL 以獨立於開倉數量的型別（`ClosingQuantity`）表示平倉數量，其值取交易所回報持倉的絕對值，且 SHALL NOT 套用步長取整。
開倉相關的函式 SHALL 只接受經取整函式建構的數量型別，使「未取整的持倉數量」在型別層級就無法用於開倉。
持倉為 0 時 SHALL 回傳「無持倉」錯誤。送單字串 SHALL 保留持倉的全部有效位數，不得截斷。

#### Scenario: 以實際持倉平倉

- **WHEN** 交易所回報持倉為 −0.019（空單）
- **THEN** 平倉數量為 0.019，不經過取整

#### Scenario: 無持倉不可平倉

- **WHEN** 交易所回報持倉為 0
- **THEN** 回傳「無持倉」錯誤

#### Scenario: 持倉數量無法用來開倉

- **WHEN** 開倉函式需要數量型別
- **THEN** 平倉數量型別無法被傳入（型別不相容，編譯期即被拒絕）

### Requirement: 配對開倉的兩腿使用同一數量

配對（pair）開倉時，系統 SHALL 為多空兩腿計算**同一個以幣為單位的數量**，名目本金 SHALL 只作為上限：
- 共同步長 SHALL 為兩腿步長（以幣為單位；OKX 為 `lotSz × ctVal`）與預設精度 `0.000001` 的最小公倍數；
- 最小量 SHALL 為兩腿最小下單量（以幣為單位）的較大者；
- 數量 SHALL 為 `名目本金 ÷ max(多腿價格, 空腿價格)` 向下取整到共同步長；
- 取整後低於最小量時 SHALL 回傳「低於最小下單量」錯誤，兩腿都 SHALL NOT 送出。
兩腿送出的數量 SHALL 相同（OKX 腿以 `數量 ÷ ctVal` 張送出，其結果 SHALL 為 `lotSz` 的整數倍）。每一腿的「數量 × 該腿價格」SHALL 不大於名目本金。
所有計算 SHALL 使用 Decimal。

#### Scenario: 不同步長取共同步長

- **WHEN** 名目本金 1,000、多腿價格 60,000（步長 0.001）、空腿價格 60,100（步長 0.0001）
- **THEN** 共同步長為 0.001，數量為 0.016（1000 ÷ 60100 = 0.01663… 向下取整），兩腿皆為 0.016，價值 960 與 961.6 皆 ≤ 1,000

#### Scenario: 步長比預設精度更細

- **WHEN** 兩腿步長皆為 0.00000001，名目本金 1,000，價格 3,000
- **THEN** 共同步長為 0.000001，數量為 0.333333

#### Scenario: 非十進位倍數的步長

- **WHEN** 多腿步長 0.002、空腿步長 0.005，名目本金 100，兩腿價格 9
- **THEN** 共同步長為 0.01，數量為 11.11（100 ÷ 9 = 11.111… 向下取整到 0.01）

#### Scenario: OKX 以張數送出

- **WHEN** OKX 腿 `ctVal` 為 0.01、`lotSz` 為 1，另一腿步長 0.001，名目本金 1,000，價格 60,000
- **THEN** 共同步長為 0.01，數量為 0.01 BTC，OKX 送出 1 張，另一腿送出 0.01

#### Scenario: 低於兩所較大的最小量

- **WHEN** 數量取整後為 0.004，兩腿最小量分別為 0.001 與 0.005
- **THEN** 回傳「低於最小下單量」，兩腿都不送出

