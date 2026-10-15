## ADDED Requirements

### Requirement: RECONCILED 配對在 T+5 至 T+15 可單腿提早平倉

狀態為 `RECONCILED` 的配對，在結算時間 `T` 之後 5 秒（含）至 15 秒（不含）的窗口內（以兩交易所校正後的時間判斷，校時缺失時 SHALL NOT 提早平倉），系統 SHALL 對**每一腿各自**判斷是否提早平倉：

- long 腿：該腿目前價格（與送單前參考價同源，單一幣種重新抓取，不使用快取）相對該腿**開倉成交價**的漲幅，大於或等於 `early_exit_min_move_pct`（等於即通過）。
- short 腿：該腿目前價格（同上）相對該腿**開倉成交價**的跌幅，大於或等於 `early_exit_min_move_pct`（等於即通過）。

達標的那一腿 SHALL 以 reduce-only 市價單單獨平倉，數量為該腿的記錄成交量與實際持倉較小者，並套用既有平倉的數量與不平衡檢查；另一腿 SHALL NOT 因此被動作。單腿平倉後，配對狀態 SHALL 維持 `RECONCILED`，且 SHALL NOT 被視為單腿失敗或不平衡。

`early_exit_min_move_pct` 未設定或為 0 時，系統 SHALL NOT 提早平倉。提早平倉 SHALL 僅在 `trigger_mode = AUTO` 且 kill switch 未啟用時進行。窗口結束（T+15）時，仍未平倉的腿 SHALL 依既有的排程平倉規則平倉；兩腿都已平倉時，配對 SHALL 進入既有的「確認已平倉」流程，不再送任何平倉單。

每一次單腿提早平倉 SHALL 寫入一筆事件，包含腿、開倉成交價、觸發時的價格、漲跌幅與門檻。

#### Scenario: 多單上漲達門檻只平多單

- **WHEN** `early_exit_min_move_pct = 0.05`、T+8 秒時 long 腿開倉價 100、目前價格 100.06，short 腿目前價格相對其開倉價不利
- **THEN** 只對 long 腿送出 reduce-only 平倉單，short 腿不動，配對狀態仍為 `RECONCILED`，並寫入一筆提早平倉事件

#### Scenario: 空單下跌達門檻只平空單

- **WHEN** `early_exit_min_move_pct = 0.05`、T+10 秒時 short 腿開倉價 100、目前價格 99.94
- **THEN** 只對 short 腿送出 reduce-only 平倉單

#### Scenario: 幅度不足不動作

- **WHEN** 多單目前價格只比開倉價高 0.03%，門檻為 0.05%
- **THEN** 不送任何平倉單，並在 T+15 照既有規則兩腿一起平倉

#### Scenario: 時間窗之外不提早平倉

- **WHEN** 現在時間為 T+3 秒，且某腿已達門檻
- **THEN** 不提早平倉；T+15 之後只走既有的排程平倉

#### Scenario: 校時缺失不提早平倉

- **WHEN** 任一腿所在交易所的校時偏移不可用
- **THEN** 不提早平倉（既有的 T+15 平倉仍照本機時間執行）

#### Scenario: 兩腿都提早平倉後進入確認

- **WHEN** long 腿在 T+6 秒提早平倉，short 腿在 T+9 秒也達門檻並平倉
- **THEN** 兩腿皆已平倉後配對進入既有的「確認已平倉」流程，T+15 不再送平倉單

#### Scenario: 單腿平倉被拒絕

- **WHEN** 提早平倉的單腿 reduce-only 單被交易所拒絕
- **THEN** 寫入事件並保持該腿未平倉，配對狀態維持 `RECONCILED`，T+15 仍依既有規則處理

#### Scenario: 手動觸發模式不提早平倉

- **WHEN** `trigger_mode = MANUAL` 且某腿已達門檻
- **THEN** 不自動提早平倉
