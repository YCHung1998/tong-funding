## ADDED Requirements

### Requirement: 以標的與交易所把兩腿持倉配成一組

系統 SHALL 以純函式把持倉列與狀態為 `RECONCILED` 的配對配起來：對每個配對，找出 long 交易所與 short 交易所上、同標的、尚未被使用的持倉列，兩者都找到才成組。
每一列 SHALL 最多被一個組使用；重複或過期的配對若指向已被使用的列，SHALL 被略過，不得重複成組。
未成組的持倉列 SHALL 原樣回傳在「未配對」清單中。

#### Scenario: 正常配成一組

- **WHEN** 有一個 `RECONCILED` 配對（BTCUSDT，long 在 Binance、short 在 Bybit），持倉中有 Binance BTCUSDT 與 Bybit BTCUSDT 各一列
- **THEN** 回傳一組包含這兩列，未配對清單為空

#### Scenario: 重複配對不重複消耗持倉

- **WHEN** 有兩個內容相同的 `RECONCILED` 配對，但只有一組持倉
- **THEN** 只成一組，第二個配對被略過

#### Scenario: 只有一腿存在

- **WHEN** 配對的 short 腿在 Bybit 上找不到持倉
- **THEN** 不成組，Binance 那一列留在未配對清單
