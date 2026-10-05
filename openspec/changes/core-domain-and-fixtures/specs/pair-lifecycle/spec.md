## ADDED Requirements

### Requirement: Pair 狀態為封閉的 enum 並保留既有名稱

Pair 狀態 SHALL 為下列封閉集合，沿用 Python 版名稱以便對照日誌：
`PREPARED`、`PRE_TRADE_CHECK`、`BLOCKED`、`ORDER_SUBMIT`、`FILL_MONITOR`、`RECONCILED`、`IMBALANCED`、`CLOSING`、`FINALIZED`、`CANCELLED`、`PARTIAL_FAILURE`、`UNRESOLVED`。
狀態轉移 SHALL 由單一純函式 `next(狀態, 事件)` 決定，非法轉移 SHALL 回傳錯誤，不得靜默忽略。

#### Scenario: 正常路徑

- **WHEN** 依序送入 開始檢查、檢查通過、兩腿送出、兩腿成交且不平衡在容許內、開始平倉、確認已平倉
- **THEN** 狀態依序為 `PRE_TRADE_CHECK`、`ORDER_SUBMIT`、`FILL_MONITOR`、`RECONCILED`、`CLOSING`、`FINALIZED`

#### Scenario: 檢查失敗

- **WHEN** 在 `PRE_TRADE_CHECK` 送入檢查失敗事件
- **THEN** 狀態為 `BLOCKED`，且 `BLOCKED` 沒有任何後續轉移

#### Scenario: 非法轉移被拒絕

- **WHEN** 在 `PREPARED` 送入「兩腿成交」事件
- **THEN** 回傳非法轉移錯誤，狀態不變

### Requirement: 合法轉移表是封閉的

`next(狀態, 事件)` SHALL 只接受下表列出的轉移，其餘一律回傳非法轉移錯誤。「系統」表示由引擎自動產生的事件，「人工」表示只能由使用者操作產生的事件。

| 自 | 事件 | 到 | 來源 |
|---|---|---|---|
| `PREPARED` | 開始檢查 | `PRE_TRADE_CHECK` | 系統 |
| `PREPARED` | 取消（條件惡化、錯過進場視窗、使用者移除） | `CANCELLED` | 系統／人工 |
| `PRE_TRADE_CHECK` | 檢查通過 | `ORDER_SUBMIT` | 系統 |
| `PRE_TRADE_CHECK` | 檢查失敗 | `BLOCKED` | 系統 |
| `ORDER_SUBMIT` | 兩腿皆已送出 | `FILL_MONITOR` | 系統 |
| `ORDER_SUBMIT` | 兩腿皆送出失敗（沒有任何曝險） | `CANCELLED` | 系統 |
| `ORDER_SUBMIT` | 一腿送出失敗、另一腿已送出 | `PARTIAL_FAILURE` | 系統 |
| `FILL_MONITOR` | 兩腿成交且不平衡在容許內 | `RECONCILED` | 系統 |
| `FILL_MONITOR` | 兩腿成交但不平衡超過容許 | `IMBALANCED` | 系統 |
| `FILL_MONITOR` | 逾時且兩腿成交量皆為 0 | `CANCELLED` | 系統 |
| `FILL_MONITOR` | 逾時且任一腿已成交而另一腿未完全成交 | `PARTIAL_FAILURE` | 系統 |
| `FILL_MONITOR` | 逾時且無法判定成交狀況 | `UNRESOLVED` | 系統 |
| `RECONCILED` | 開始平倉（排程或人工） | `CLOSING` | 系統／人工 |
| `CLOSING` | 兩腿已平倉且確認持倉為 0、無未成交委託 | `FINALIZED` | 系統 |
| `CLOSING` | 任一腿平倉失敗或僅部分平倉 | `PARTIAL_FAILURE` | 系統 |
| `ORDER_SUBMIT`、`FILL_MONITOR`、`CLOSING` | 重啟對帳發現單腿成交或部分狀態 | `PARTIAL_FAILURE` | 系統 |
| `ORDER_SUBMIT`、`FILL_MONITOR`、`CLOSING` | 重啟對帳無法判定 | `UNRESOLVED` | 系統 |
| `PARTIAL_FAILURE`、`IMBALANCED`、`UNRESOLVED` | 人工要求平倉 | `CLOSING` | 人工 |
| `PARTIAL_FAILURE`、`IMBALANCED`、`UNRESOLVED` | 人工確認已平倉（附已驗證兩腿持倉為 0 且無未成交委託） | `FINALIZED` | 人工 |

`BLOCKED`、`CANCELLED`、`FINALIZED` SHALL 為終止狀態，沒有任何轉出。

#### Scenario: 表中未列出的轉移被拒絕

- **WHEN** 在 `RECONCILED` 送入「兩腿皆已送出」事件
- **THEN** 回傳非法轉移錯誤，狀態不變

#### Scenario: 兩腿皆送出失敗不留下曝險狀態

- **WHEN** 在 `ORDER_SUBMIT` 送入「兩腿皆送出失敗」
- **THEN** 狀態為 `CANCELLED`

#### Scenario: 成交後不平衡超標

- **WHEN** 在 `FILL_MONITOR` 送入「兩腿成交但不平衡超過容許」
- **THEN** 狀態為 `IMBALANCED`，且沒有任何自動補單事件

#### Scenario: 平倉失敗轉人工

- **WHEN** 在 `CLOSING` 送入「一腿平倉失敗」
- **THEN** 狀態為 `PARTIAL_FAILURE`

#### Scenario: 重啟對帳發現單腿成交

- **WHEN** 在 `FILL_MONITOR` 送入「重啟對帳發現單腿成交」
- **THEN** 狀態為 `PARTIAL_FAILURE`

#### Scenario: 人工確認已平倉但未附驗證

- **WHEN** 在 `PARTIAL_FAILURE` 送入「人工確認已平倉」，但驗證欄位為否
- **THEN** 回傳非法轉移錯誤，狀態維持 `PARTIAL_FAILURE`

### Requirement: 單腿失敗與不平衡不得自動轉出

`PARTIAL_FAILURE`、`IMBALANCED`、`UNRESOLVED` 三個狀態 SHALL 沒有任何由系統自動事件觸發的轉出。
只有明確標示為人工的事件（人工要求平倉、人工確認已平倉）才能使其離開。
此規則 SHALL 由測試對「所有非人工事件」逐一驗證。

#### Scenario: 一腿成交另一腿失敗

- **WHEN** 在 `ORDER_SUBMIT` 送入「一腿送出失敗、另一腿已送出」
- **THEN** 狀態為 `PARTIAL_FAILURE`，成功那一腿的資料被保留

#### Scenario: 系統事件無法讓它離開

- **WHEN** 在 `PARTIAL_FAILURE` 依序送入 逾時、重試、重新檢查、平倉成功 等所有非人工事件
- **THEN** 每一次都回傳非法轉移錯誤，狀態維持 `PARTIAL_FAILURE`

#### Scenario: 人工要求平倉才能離開

- **WHEN** 在 `PARTIAL_FAILURE` 送入「人工要求平倉」
- **THEN** 狀態為 `CLOSING`

### Requirement: 逾時依雙腿成交狀況分流

在 `FILL_MONITOR` 逾時時：兩腿皆未成交 SHALL 轉為 `CANCELLED`；任一腿已成交而另一腿未完全成交 SHALL 轉為 `PARTIAL_FAILURE`；
無法判定成交狀況 SHALL 轉為 `UNRESOLVED`。系統 SHALL NOT 自動補買或補賣來湊齊對腿。

#### Scenario: 兩腿皆未成交

- **WHEN** 逾時且兩腿成交量皆為 0
- **THEN** 狀態為 `CANCELLED`

#### Scenario: 一腿完成、另一腿 70%

- **WHEN** 逾時且 long 已 100%、short 僅 70%
- **THEN** 狀態為 `PARTIAL_FAILURE`，不產生任何自動補單事件

### Requirement: FINALIZED 須確認已平倉

進入 `FINALIZED` 的事件 SHALL 附帶「兩腿持倉皆為 0 且無未成交委託」的確認；缺少或為否時 SHALL 拒絕轉移。

#### Scenario: 缺少已平倉確認

- **WHEN** 在 `CLOSING` 送入「完成」事件但確認為否
- **THEN** 回傳非法轉移錯誤，狀態維持 `CLOSING`
