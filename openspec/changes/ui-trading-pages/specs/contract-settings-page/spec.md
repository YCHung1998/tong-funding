## ADDED Requirements

### Requirement: 預設模板以每腿名目本金定義

合約設定頁 SHALL 提供預設下單模板，欄位為 Target Notional（USDT）與 Leverage；Target Notional SHALL 定義為「每一腿」的名目本金，頁面 SHALL 以文字明示，並 SHALL 同時顯示「一對 LONG + SHORT」的總名目本金與合計保證金。
Target Notional 與 Leverage 皆 SHALL 大於 0；不合法的輸入 SHALL 顯示具體錯誤並禁止儲存。
儲存 SHALL 持久化模板並寫入 `CONTRACT_SETTINGS_UPDATED` 事件；儲存 SHALL NOT 修改任何既有持倉或既有暫存配對。
Leverage 超過全域 `max_leverage` 或任一交易所覆寫的 `max_leverage` 時，頁面 SHALL 顯示警告（不阻擋儲存，因為送單前檢查的 `Leverage` 才是最終把關）。
模板的初始預設值 SHALL 為 Target Notional 1000 USDT、Leverage 5（沿用 Python 版 `contract_settings.py`）；Figma 的 1,200 與 3× 是示範值。

#### Scenario: 總額文字手算一致

- **WHEN** Target Notional 為 1,200、Leverage 為 3
- **THEN** 頁面顯示一對配對的總名目本金 2,400 USDT 與合計保證金 800 USDT

#### Scenario: 非法輸入禁止儲存

- **WHEN** 使用者輸入 Target Notional 為 0
- **THEN** 顯示錯誤並禁用儲存，已儲存的模板不變

#### Scenario: 槓桿高於上限只警告

- **WHEN** 全域 `max_leverage` 為 5，使用者輸入 Leverage 為 8
- **THEN** 頁面顯示超過上限的警告，儲存仍可進行

#### Scenario: 儲存不影響既有持倉

- **WHEN** 已有開啟中的配對，使用者儲存新模板
- **THEN** 既有配對的數量與槓桿不變，新模板只套用於之後新加入的候選

### Requirement: 槓桿與保證金雙向連動

頁面 SHALL 提供兩種計算模式：「用槓桿反推保證金」與「用保證金反推槓桿」。
前者 SHALL 以 Margin = Notional ÷ Leverage（每腿）計算；後者 SHALL 以 Leverage = Notional ÷ Margin 計算。
計算 SHALL 使用 Decimal，SHALL NOT 以浮點誤差容忍值補償。
保證金或槓桿為 0 或負值時 SHALL 顯示錯誤，SHALL NOT 計算或儲存。
儲存的規範值 SHALL 為 Target Notional 與 Leverage；保證金為由此導出的顯示值。

#### Scenario: 槓桿反推保證金

- **WHEN** Notional 為 1,200、Leverage 為 3
- **THEN** 每腿 Margin 為 400.00 USDT，並顯示算式「1,200 ÷ 3 = 400 / leg」

#### Scenario: 保證金反推槓桿

- **WHEN** 切到「用保證金反推槓桿」，Notional 為 1,200、輸入 Margin 為 400
- **THEN** Leverage 顯示為 3

#### Scenario: 保證金為 0 被拒絕

- **WHEN** 在保證金模式輸入 Margin 為 0
- **THEN** 顯示錯誤，Leverage 不被更新

### Requirement: 數量試算須顯示取整後的數量

合約試算 SHALL 對使用者輸入的標的，在每個列出的交易所顯示：目前價格、價格取得時間與新鮮度、依模板計算的預期數量。
預期數量 SHALL 先以「每腿 Notional ÷ 現價」得到目標幣量，再依該所的 `step_size` 向下取整（OKX SHALL 先以 `ct_val` 換算為合約張數再套用 `lotSz`），頁面 SHALL 顯示取整後的數量與單位，SHALL NOT 以未取整的六位小數作為預期數量。
OKX 的數量 SHALL 以「合約張數」為單位顯示，並同時顯示對應的幣量。
取整後低於最小下單量時，SHALL 顯示「低於最小下單量」，且 SHALL NOT 顯示 0 或原數量。
價格已過期（超過 `stale_data_threshold_ms`）或取不到合約規格（`step_size`、`min_qty`、`ct_val`）時，SHALL 顯示原因並不顯示數量，SHALL NOT 以預設值或舊值計算。
頁面 SHALL 說明試算未包含成交滑價與手續費。

#### Scenario: 依步長向下取整

- **WHEN** 每腿 Notional 為 1,200 USDT、現價 60,200、`step_size` 為 0.001
- **THEN** 預期數量顯示為 0.019 BTC

#### Scenario: OKX 以張數顯示

- **WHEN** 目標幣量為 0.021 BTC、`ct_val` 為 0.01、`lotSz` 與 `minSz` 皆為 1
- **THEN** OKX 預期數量顯示為 2 張（對應 0.02 BTC）

#### Scenario: 低於最小下單量

- **WHEN** 每腿 Notional 換算的幣量為 0.0004、`step_size` 與 `min_qty` 皆為 0.001
- **THEN** 顯示「低於最小下單量」，不顯示數量

#### Scenario: 價格過期不給數量

- **WHEN** 某所價格的取得時間超過 `stale_data_threshold_ms`
- **THEN** 該所顯示「價格已過期」並不顯示預期數量

#### Scenario: 取不到合約規格

- **WHEN** 某所的 `step_size` 無法取得
- **THEN** 該所顯示「無法取得合約規格」並不顯示預期數量

### Requirement: 計算與執行邊界表

頁面 SHALL 顯示「計算與執行邊界」表，列出每腿與雙腿合計的 Notional、Initial Margin 與預期數量，並標示目前的 `execution_mode`。
雙腿合計 SHALL 為每腿值的兩倍；預期數量欄 SHALL 標示為「LONG / SHORT 各一腿」，不做跨所加總。
表下 SHALL 註明實際下單數量依各所 lot size 取整、並由送單前檢查以最新資料重新驗證。

#### Scenario: 合計為每腿兩倍

- **WHEN** 每腿 Notional 為 1,200、Margin 為 400
- **THEN** 表中雙腿合計為 Notional 2,400.00、Initial Margin 800.00

#### Scenario: 模式標示

- **WHEN** `execution_mode` 為 `EXCHANGE_DEMO`
- **THEN** 邊界表標示 `EXCHANGE_DEMO`
