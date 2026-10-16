## ADDED Requirements

### Requirement: Live 身分每次啟動 kill switch 為 ON

Live 身分 SHALL 在每次啟動時（包含當機後重啟）把 kill switch 設為 ON 並寫入資料庫，不論先前的值。kill switch 為 ON 時，系統 SHALL 拒絕所有會開倉的命令（進場、加入交易單的自動觸發、手動開倉、補單），SHALL 照常允許平倉與取消。Demo 身分的啟動行為 SHALL 不變。

#### Scenario: Live 啟動即鎖定

- **WHEN** Live 身分啟動，資料庫裡的 kill switch 先前為 OFF
- **THEN** 啟動完成後 kill switch 為 ON，開倉命令被拒絕，平倉命令被接受

#### Scenario: Demo 啟動不被鎖定

- **WHEN** Demo 身分啟動，kill switch 先前為 OFF
- **THEN** kill switch 維持 OFF

### Requirement: 武裝是引擎指令且需要預檢與確認

解除 Live 的 kill switch SHALL 只能透過引擎指令 `Arm { preflight_id, caps_hash, confirmation, checklist_ack }`。引擎 SHALL 驗證：`preflight_id` 是引擎自己在最近 60 秒內產生且全部通過的預檢結果；`caps_hash` 等於當下儲存的上限（`live_max_leg_notional_usdt`、`live_max_pairs_per_arming`、`max_concurrent_pairs`、`max_leverage`）的雜湊；`confirmation` 完全等於固定確認字串（比對前做 Unicode NFC 正規化並去除前後空白）；`checklist_ack` 為真。任一不符 SHALL 拒絕並說明原因，kill switch 維持 ON。成功時 SHALL 把 kill switch 設為 OFF、把上限凍結在記憶體、把本次武裝的進場筆數歸零，並寫入 `LIVE_ARMED` 事件。Demo 身分 SHALL 拒絕 `Arm`。

#### Scenario: 缺少或過期的預檢

- **WHEN** 送出 `Arm` 但 `preflight_id` 不存在、不是引擎產生的，或已超過 60 秒
- **THEN** 被拒絕，kill switch 維持 ON

#### Scenario: 預檢有失敗項目

- **WHEN** 預檢結果中任一項為失敗
- **THEN** 無法取得可用於 `Arm` 的 `preflight_id`

#### Scenario: 確認字串不符

- **WHEN** `confirmation` 與固定字串不完全相同
- **THEN** 被拒絕

#### Scenario: 上限在預檢後被改動

- **WHEN** 預檢之後、`Arm` 之前上限被修改，使 `caps_hash` 不符
- **THEN** 被拒絕，需重新預檢

#### Scenario: 武裝成功

- **WHEN** 預檢全綠且確認字串與勾選清單都正確
- **THEN** kill switch 為 OFF、本次武裝進場筆數為 0、寫入 `LIVE_ARMED` 事件

### Requirement: 預檢項目與失敗即封閉

預檢 SHALL 至少包含：兩所 live 金鑰存在；兩所 live 唯讀餘額與持倉讀取成功；兩所時鐘已校時；兩所為單向持倉模式；兩所帳戶沒有未平倉部位與掛單；可用保證金足以支付上限所需的保證金；`live_max_leg_notional_usdt` 與 `live_max_pairs_per_arming` 已設定且在天花板內；資料庫可寫且磁碟可用空間不低於 100 MB。任何一項讀取失敗或無法判定 SHALL 視為失敗。預檢結果 SHALL 只保存各項的通過／失敗與原因文字，SHALL NOT 保存交易所的原始回應或金鑰片段。

#### Scenario: 讀取失敗視為失敗

- **WHEN** 其中一所的持倉讀取逾時
- **THEN** 該項為失敗，預檢整體不通過

#### Scenario: 帳戶有既有部位

- **WHEN** 任一所帳戶有未平倉部位或掛單
- **THEN** 預檢不通過並指出是哪一所

### Requirement: Live 硬上限由引擎強制

風控設定 SHALL 新增 `live_max_leg_notional_usdt` 與 `live_max_pairs_per_arming`（皆為 live 必填、無預設值）；程式 SHALL 內建絕對天花板（本版：每腿名目 ≤ 100 USDT、每次武裝筆數 ≤ 5），設定超過天花板 SHALL 被拒絕。Live 身分下，所有非 reduce-only 的送單 SHALL 在訂單請求建立後以「數量 × 最差保護價」檢查不得超過 `live_max_leg_notional_usdt`，超過 SHALL 拒絕送出；加入交易單、儲存合約模板與進場前檢查 SHALL 同樣檢查。reduce-only 平倉單 SHALL NOT 受此限制。

#### Scenario: 超過上限的進場被拒

- **WHEN** Live 身分武裝後，配對每腿名目 25 USDT，上限為 10 USDT
- **THEN** 進場被拒絕，沒有任何訂單送出

#### Scenario: 合約模板不能超過上限

- **WHEN** Live 身分儲存名目 1000 USDT 的合約模板
- **THEN** 被拒絕並說明上限

#### Scenario: 設定不能超過天花板

- **WHEN** 把 `live_max_leg_notional_usdt` 設為 500
- **THEN** 被拒絕並說明程式內建天花板

#### Scenario: 平倉不受上限影響

- **WHEN** 持有名目超過上限的部位（例如價格上漲後），送出 reduce-only 平倉
- **THEN** 平倉被接受

### Requirement: 一次武裝的進場筆數上限

Live 身分 SHALL 在每次武裝後累計進場（開倉單送出）的配對數；累計達 `live_max_pairs_per_arming` 時 SHALL 自動把 kill switch 設為 ON 並寫 `LIVE_AUTO_LOCK` 事件（原因為筆數已達上限），要再交易 SHALL 重新武裝。

#### Scenario: 第 2 筆之後自動鎖定

- **WHEN** `live_max_pairs_per_arming = 2`，武裝後第二個配對進場
- **THEN** 第二筆照常完成，之後 kill switch 為 ON，第三個配對的進場被拒絕

### Requirement: Live 手動下單頁停用開倉並提供單腿平倉

Live 身分下，引擎 SHALL 拒絕所有非 reduce-only 的手動單；reduce-only 手動單與取消 SHALL 照常。引擎 SHALL 提供 `FlattenLeg { pair, leg }`：以 reduce-only 市價單平掉指定配對的單一腿，數量為該腿記錄成交量與實際持倉較小者，沿用既有平倉的數量與不平衡檢查，且 kill switch 為 ON 時 SHALL 允許。

#### Scenario: 手動開倉被拒

- **WHEN** Live 身分送出一筆非 reduce-only 的手動單
- **THEN** 被拒絕並說明 live 停用手動開倉

#### Scenario: 平掉裸露的一腿

- **WHEN** 配對為 `PARTIAL_FAILURE`，只有 long 腿成交，使用者送出 `FlattenLeg { leg: long }`
- **THEN** 只對 long 腿送出 reduce-only 平倉單，數量為實際持倉

### Requirement: 自動鎖定條件

Live 身分下，下列事件發生時系統 SHALL 自動把 kill switch 設為 ON 並寫 `LIVE_AUTO_LOCK` 事件（含原因）：任何配對進入 `PARTIAL_FAILURE`、`IMBALANCED` 或 `UNRESOLVED`；認證類錯誤（HTTP 401／403，或交易所明確回報金鑰／簽名／IP 錯誤）；HTTP 418 或 429；進場前時鐘校時失敗。自動鎖定 SHALL 不阻擋既有部位的平倉與取消。

#### Scenario: 單腿失敗自動鎖定

- **WHEN** 配對進入 `PARTIAL_FAILURE`
- **THEN** kill switch 為 ON，寫入 `LIVE_AUTO_LOCK`，人工平倉仍可執行

#### Scenario: 被限頻自動鎖定

- **WHEN** 任一所回應 HTTP 418
- **THEN** kill switch 為 ON，寫入 `LIVE_AUTO_LOCK`

### Requirement: 武裝期間上限凍結且命令列不得寫入 live 上限

武裝後，系統 SHALL 以武裝當下凍結的上限值執行；調高任何上限的設定變更 SHALL 被拒絕（需先把 kill switch 設為 ON 再重新武裝），調低 SHALL 立即生效。`config set` 命令列工具 SHALL 拒絕對 live 資料庫寫入 `live_max_leg_notional_usdt`、`live_max_pairs_per_arming`、`max_leverage`、`max_concurrent_pairs` 與 `trigger_mode`。

#### Scenario: 武裝中調高上限被拒

- **WHEN** 已武裝，使用者把 `live_max_leg_notional_usdt` 由 10 改為 20
- **THEN** 被拒絕，並說明需先鎖定再重新武裝

#### Scenario: 命令列不能改 live 上限

- **WHEN** 對 live 資料庫執行 `config set live_max_leg_notional_usdt 20`
- **THEN** 命令失敗，資料庫沒有任何變更

### Requirement: Live 資料庫初始值

新建的 live 資料庫 SHALL 以 `trigger_mode = MANUAL`、`early_exit_min_move_pct = 0` 初始化，合約模板名目 SHALL NOT 超過上限；必填上限未設定時 SHALL 無法取得通過的預檢。Live 身分 SHALL NOT 允許把 `trigger_mode` 改為 `AUTO`，直到本變更之後的「自動交易準備」變更啟用為止。

#### Scenario: 新 live 資料庫的初始值

- **WHEN** 第一次以 `--live` 啟動，建立新的 live 資料庫
- **THEN** `trigger_mode` 為 `MANUAL`、`early_exit_min_move_pct` 為 0

#### Scenario: Live 不能切到 AUTO

- **WHEN** Live 身分嘗試把觸發模式改為 `AUTO`
- **THEN** 被拒絕

### Requirement: 前往交易所連結

Live 身分下，橫幅與每個配對卡 SHALL 提供「前往交易所」按鈕，以預設瀏覽器開啟該幣在 Binance 合約與 Bybit 合約的網頁交易介面；開啟的網址 SHALL 只包含公開資訊（交易所網域與交易對），SHALL NOT 含任何金鑰或簽名。

#### Scenario: 連結只含公開資訊

- **WHEN** 產生某配對的 Binance 與 Bybit 連結
- **THEN** 連結的網域為交易所官方網域，路徑含交易對，不含查詢字串中的金鑰或簽名
