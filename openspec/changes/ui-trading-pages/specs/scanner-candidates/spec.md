## ADDED Requirements

### Requirement: 掃幣表的「加入交易單」欄

掃幣頁的 Funding Rate Matrix SHALL 有一欄「加入交易單」，每列一個勾選框；勾選即把該列放入 Candidate List，取消勾選即移出。
勾選框 SHALL 只在下列條件全部成立時可用，否則 SHALL 禁用並顯示第一個不成立的原因（原因為列舉，不是自由字串）：
- 該列有最佳套利方向，且兩腿皆為可下單交易所（Binance、Bybit；OKX 僅比價）；
- 該列「達標」（由 core 判定的 Net Edge 達 `net_edge_threshold_pct`，且預期淨收益達 `min_expected_net_pnl_pct`）；設定不完整時視為不可判定而禁用；
- 兩腿的行情來源最近一次抓取沒有失敗（舊資料不得成為候選）；
- 兩腿的結算時間已知且尚未到達；
- 該標的目前沒有 `PREPARED` 或進行中的配對。
勾選 SHALL NOT 寫入任何資料、SHALL NOT 送出任何 engine 命令；Candidate List 只存在於本次執行的記憶體中，程式重啟後為空。

#### Scenario: 達標列可勾選

- **WHEN** 某列達標、方向為 L Binance / S Bybit、兩腿行情新鮮、結算在未來
- **THEN** 勾選框可用，勾選後該列出現在 Candidate List，engine 收到 0 個命令

#### Scenario: 未達標或含 OKX 的列不可勾選

- **WHEN** 某列未達標，或其最佳方向含 OKX
- **THEN** 勾選框禁用並顯示原因（「未達標」或「OKX 僅比價」）

#### Scenario: 已有暫存配對的標的

- **WHEN** BTCUSDT 已有 `PREPARED` 配對
- **THEN** BTCUSDT 列的勾選框禁用並顯示「已在交易單」

### Requirement: Candidate List 逐筆顯示將建立的配對

Candidate List SHALL 列出每個候選：標的、long / short 交易所、Gross Spread、Net Edge、結算時間（UTC）與倒數、依目前合約模板的每腿 Notional、槓桿與每腿 Margin。
Candidate List SHALL 在資料更新後重新判定每個候選的可用性；不再符合「加入交易單」條件的候選 SHALL 標示原因且 SHALL NOT 被加入。
合約模板不合法或不存在時，SHALL 顯示原因並禁用加入。

#### Scenario: 候選顯示模板數值

- **WHEN** 合約模板為 Notional 1,200、Leverage 3，Candidate List 有 1 筆
- **THEN** 該筆顯示每腿 Notional 1,200.00、槓桿 3×、每腿 Margin 400.00

#### Scenario: 候選失效

- **WHEN** 候選加入後，該列因新資料變為未達標
- **THEN** 該候選標示「未達標」，「加入並前往交易單」不會把它送出

### Requirement: 加入並前往交易單只建立 PREPARED 配對

「加入並前往交易單 →」SHALL 對每個仍有效的候選送出一個 engine 的 `AddPrepared` 命令，內容 SHALL 包含：新的 `internal_uuid` 與 `pair_id`、標的、long / short 交易所、結算時間 `T`（兩腿中較早的 `next_funding_time`）、以及掃描當下的快照（兩腿掃描價格、每腿 Notional、槓桿、Gross Spread、Net Edge %）。
此動作 SHALL NOT 送出任何訂單；配對只進入 `PREPARED`，之後由交易單頁（或 AUTO 排程器）決定是否進場。
engine 回覆 `AlreadyPending` 或拒絕時，SHALL 如實顯示該候選的結果並保留於 Candidate List；接受的候選 SHALL 自清單移除，頁面 SHALL 切換到交易單頁。

#### Scenario: 加入兩個候選

- **WHEN** Candidate List 有 2 個有效候選，使用者按「加入並前往交易單」
- **THEN** engine 恰好收到 2 個 `AddPrepared` 命令、0 個下單命令，頁面切到交易單頁

#### Scenario: 標的已有暫存配對

- **WHEN** engine 對某候選回覆 `AlreadyPending`
- **THEN** 該候選留在 Candidate List 並顯示「已有暫存配對」
