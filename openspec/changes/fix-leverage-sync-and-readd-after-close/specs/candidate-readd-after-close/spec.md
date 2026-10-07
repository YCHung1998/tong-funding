## ADDED Requirements

### Requirement: A pair without exposure does not block re-adding its symbol
The scanner SHALL NOT report a symbol as already staged because of a pair that has no exposure left. A pair has no exposure left when (a) it is `CLOSING` and its flat confirmation (`CLOSE_CONFIRMED`) has been recorded, or (b) it is `PARTIAL_FAILURE`, `IMBALANCED` or `UNRESOLVED` and, for both legs' exchanges, the account position list and open-order list were read completely without error and contain no position and no open order on the symbol, and the read is at most 45 seconds old.

#### Scenario: Closed and waiting for funding PnL
- **WHEN** a pair is `CLOSING` with the flat confirmation recorded and the symbol qualifies in the scan
- **THEN** the row can be ticked

#### Scenario: Locked pair flattened by manual orders
- **WHEN** a pair is `PARTIAL_FAILURE`, both legs' accounts report no position and no open order on the symbol
- **THEN** the row can be ticked

#### Scenario: Position still open
- **WHEN** a pair is `PARTIAL_FAILURE` and one leg's account still holds a position on the symbol
- **THEN** the row shows "已在交易單" and cannot be ticked

#### Scenario: Account read unknown
- **WHEN** a pair is `IMBALANCED` and a leg's position or open-order read failed, is incomplete, is older than 45 seconds, or was never made
- **THEN** the symbol is still treated as staged

### Requirement: Exposure-bearing and in-progress pairs still block
`PREPARED`, `PRE_TRADE_CHECK`, `ORDER_SUBMIT`, `FILL_MONITOR`, `RECONCILED`, and `CLOSING` without a recorded flat confirmation SHALL keep blocking their symbol.

#### Scenario: Closing not yet confirmed flat
- **WHEN** a pair is `CLOSING` and no flat confirmation is recorded
- **THEN** the row shows "已在交易單"

### Requirement: The finished pair is left unchanged
Making its symbol tickable again SHALL NOT change the earlier pair's state, alerts or its funding PnL wait.

#### Scenario: Old pair keeps its state
- **WHEN** the symbol of a flat `PARTIAL_FAILURE` pair is added again
- **THEN** the earlier pair is still `PARTIAL_FAILURE` with its alert until the user confirms it closed

### Requirement: A flat-confirmed closing pair frees its concurrent-pair slot
A `CLOSING` pair whose flat confirmation is recorded and that only waits for its funding PnL SHALL NOT count toward `max_concurrent_pairs` in the pre-trade check of another pair.

#### Scenario: Re-added symbol passes the pair limit
- **WHEN** `max_concurrent_pairs` is 1, an earlier pair is `CLOSING` with the flat confirmation recorded, and a new pair on the same symbol reaches its pre-trade check
- **THEN** the pre-trade check does not fail the pair limit and the pair proceeds

#### Scenario: Unconfirmed closing pair still counts
- **WHEN** an earlier pair is `CLOSING` without a flat confirmation
- **THEN** it still takes a slot
