## Why

一腿成交、另一腿沒成交（`PARTIAL_FAILURE`／`IMBALANCED`／`UNRESOLVED`）時，系統目前只會鎖定與通知，**裸露的那一腿要等人手動平倉**；人不在場時，方向風險可以一直存在。我們挑的是資金費極端、波動大、深度薄的幣，這種情況最容易發生在結算前後幾秒內。
使用者已決定採用**授權 B**：只在「確認只有一腿成交」時由系統自動平掉那一腿，並要求把這條規則**清楚寫在頁面上**（2026-10-22）。

## What Changes

- **單腿看門狗**：配對進入 `PARTIAL_FAILURE`／`IMBALANCED`／`UNRESOLVED` 後等待緩衝時間（5 秒；進入此狀態前已等過訂單逾時），逐腿查證：以 `client_order_id` 查每張單的成交量與是否已終結、讀交易所持倉。
  - **確認只有一腿有部位、另一腿沒有成交也沒有部位** → 以 reduce-only 市價單平掉有部位的那一腿（數量 = 記錄成交量與持倉的較小者，沿用既有 `plan_leg_close` 的容差檢查）；最多送出 3 次。
  - **兩腿都有部位**（可能不平衡）或**兩腿都沒有部位** → 不送單，只更新提示。
  - **查不清楚**（查詢失敗、找不到單、持倉讀不到、持倉與記錄不符、委託仍未終結且撤單後仍未終結）或**重試用完** → 不送單，停止開新倉（kill switch ON）並通知。
  - 永遠不開新倉、不加碼、不碰配對以外的部位；**不改變配對狀態**（仍須人工確認已平倉才會 FINALIZED，符合 `pair-lifecycle`「單腿失敗不得自動轉出」）。
- Demo 與 Live 都啟用（Demo 用 demo 帳戶驗證行為）；手動的「平掉裸露的一腿」按鈕維持現狀。
- **頁面揭露授權 B**：風控設定頁新增固定顯示（不可收合）的「自動處理規則（授權 B）」區塊；交易單的人工處理卡顯示看門狗的狀態與下一步；通知文字說明系統做了什麼。
- 新事件：`NAKED_LEG_WATCHDOG`（每次查證的結論與依據）；平倉沿用 `LEG_FLATTEN`，標記 `by: "watchdog"`。

## Capabilities

### New Capabilities
- `naked-leg-watchdog`: 單腿裸露的自動查證與平倉（授權 B）、重試與失敗時的鎖定、頁面揭露。

### Modified Capabilities
（無：`pair-lifecycle` 的狀態轉移表不變，看門狗不改變配對狀態。）

## Impact

- `app/src/engine/`：看門狗排程與查證（重用 `recorded_fill`、`signed_position`、`plan_leg_close`、`close_request`、`land_intents`）；`actor/live.rs` 的平倉路徑抽成系統與 Live 手動共用；決策邏輯放純函式並測試。
- `app/src/ui/`：風控設定頁的授權 B 區塊、人工處理卡文字、通知文字。
- 不改 Net Edge、進場、排程出場；不新增設定欄位（緩衝 5 秒、重試 3 次為常數，頁面顯示實際值）。
