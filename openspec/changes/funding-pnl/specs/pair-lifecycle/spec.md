## MODIFIED Requirements

### Requirement: FINALIZED 須確認已平倉

進入 `FINALIZED` 的事件 SHALL 同時附帶兩項確認：「兩腿持倉皆為 0 且無未成交委託」，以及「PnL 已計算」（指向該配對的 `PAIR_PNL_COMPUTED` 或 `PAIR_PNL_RECOMPUTED` 事件，不論其狀態為 `COMPLETE` 或 `INCOMPLETE`）；任一項缺少或為否時 SHALL 拒絕轉移。
經由人工平倉（`PARTIAL_FAILURE`、`IMBALANCED`、`UNRESOLVED` 之後的 `CLOSING`）進入 `FINALIZED` 的配對 SHALL 受相同限制。
`SIMULATION` 配對依 pnl-accounting 不產生 PnL，其「PnL 已計算」確認 SHALL 為明示的「不適用（模擬）」，且 SHALL 只用於模擬配對；EXCHANGE_DEMO 配對 SHALL NOT 以此代替 PnL 事件。

#### Scenario: 缺少已平倉確認

- **WHEN** 在 `CLOSING` 送入「完成」事件但已平倉確認為否
- **THEN** 回傳非法轉移錯誤，狀態維持 `CLOSING`

#### Scenario: 缺少 PnL 已計算確認

- **WHEN** 在 `CLOSING` 送入「完成」事件，已平倉確認為是，但沒有 PnL 已計算確認
- **THEN** 回傳非法轉移錯誤，狀態維持 `CLOSING`

#### Scenario: 兩項確認齊全

- **WHEN** 在 `CLOSING` 送入「完成」事件，已平倉確認與 PnL 已計算確認皆為是
- **THEN** 狀態為 `FINALIZED`

#### Scenario: 人工平倉的配對同樣需要 PnL

- **WHEN** 配對由 `PARTIAL_FAILURE` 經人工要求平倉進入 `CLOSING`，之後送入已平倉確認為是、但沒有 PnL 已計算確認的「完成」事件
- **THEN** 回傳非法轉移錯誤，狀態維持 `CLOSING`

#### Scenario: 模擬配對以「不適用」確認

- **WHEN** `SIMULATION` 配對在 `CLOSING` 送入「完成」事件，已平倉確認為是、PnL 確認為「不適用（模擬）」
- **THEN** 狀態為 `FINALIZED`，且沒有寫入任何 PnL 事件
