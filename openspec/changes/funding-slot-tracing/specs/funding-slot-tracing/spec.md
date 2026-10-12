## ADDED Requirements

### Requirement: A persistent slot per held settlement
For every non-simulated pair (Binance, Bybit or OKX legs), the system SHALL keep one funding slot per (pair, leg, settlement time `t`) for each settlement time on that leg's funding grid with `opened < t ≤ closed` (or `≤ now` while open), when its own records show the position was held. Slots SHALL be stored in the database and survive restarts. A pair closed before `t`, and a simulated pair SHALL have no slot for it.

#### Scenario: Position held through a settlement
- **WHEN** a pair holds both legs across a settlement time
- **THEN** each leg has a slot for that time in state `TRACING_1`

#### Scenario: Closed before the settlement
- **WHEN** the pair is flat before `t`
- **THEN** no slot exists for `t`

#### Scenario: Slots survive a restart
- **WHEN** the application restarts with slots in tracing
- **THEN** the same slots, states and next-check times are read back

### Requirement: Staged tracing
A slot SHALL be checked at `t+5, +10, … +30` minutes in `TRACING_1`, then at `t+40, +50, +60` minutes in `TRACING_2`. A check that finds the ledger row SHALL set the slot `SETTLED`. A slot still unfound at `t+30` SHALL move to `TRACING_2`, and at `t+60` to `MANUAL_CHECK`. The stage SHALL follow the elapsed time since `t`.

#### Scenario: Found in stage 1
- **WHEN** the row appears at `t+4.5 min` and the `t+5` check runs
- **THEN** the slot is `SETTLED` with the amount and the exchange ledger id

#### Scenario: Found in stage 2
- **WHEN** the row is first present at the `t+40` check
- **THEN** the slot is `SETTLED`

#### Scenario: Never found
- **WHEN** all checks up to `t+60` completed and found nothing
- **THEN** the slot is `MANUAL_CHECK` with reason `NOT_FOUND`

### Requirement: A zero funding rate settles as zero
When a completed check finds no ledger row, the system SHALL read the exchange's public funding-rate history for the slot's settlement time. A rate of exactly 0 SHALL settle the slot immediately with amount 0 and resolution `ZERO_RATE`, storing the rate and its source. A non-zero rate with no ledger row SHALL NOT settle the slot. A failed rate lookup SHALL change nothing. A ledger row found later or earlier SHALL take precedence (resolution `LEDGER`).

#### Scenario: Rate 0, no row
- **WHEN** the check at `t+5` finds no row and the public history shows rate 0 at `t`
- **THEN** the slot is `SETTLED` with amount 0 and resolution `ZERO_RATE`

#### Scenario: Rate not 0, no row
- **WHEN** no row exists and the history shows a non-zero rate
- **THEN** the slot keeps tracing and ends in `MANUAL_CHECK` with reason `NOT_FOUND` if the row never comes

#### Scenario: Rate lookup fails
- **WHEN** the history request fails
- **THEN** the slot's state is decided by the ordinary rules only

#### Scenario: Zero-amount row exists
- **WHEN** the exchange returns a ledger row with amount 0
- **THEN** the slot settles with resolution `LEDGER`

### Requirement: A failed query is not an absence
A check whose query failed (network, rate limit, missing keys, incomplete page) SHALL NOT count as "nothing found". If the checks at and after the deadline all failed, the slot SHALL become `MANUAL_CHECK` with reason `FETCH_FAILED`; it SHALL NOT be reported as `NOT_FOUND`.

#### Scenario: Offline at the deadline
- **WHEN** the checks from `t+40` to `t+60` all fail
- **THEN** the slot is `MANUAL_CHECK` with reason `FETCH_FAILED`

### Requirement: Catch-up after sleep or restart
On start and after a gap in the loop, every overdue slot SHALL be checked once immediately, before its stage is decided from the elapsed time. A slot older than `t+60 min` SHALL get that one check before becoming `MANUAL_CHECK`.

#### Scenario: Machine slept through the whole window
- **WHEN** the application wakes at `t+80 min` and the row exists on the exchange
- **THEN** the immediate check links it and the slot is `SETTLED`, not `MANUAL_CHECK`

### Requirement: The match is linked to the pair
When a slot is settled the system SHALL store the pair id, leg, settlement time, amount, exchange ledger id, the ledger dedupe key and the first-seen time, and SHALL append a `PAIR_FUNDING_LINKED` event carrying the same facts and the pair id. A ledger row SHALL be linked to at most one slot. A match that is ambiguous between two pairs SHALL NOT be linked.

#### Scenario: Linked once
- **WHEN** the same ledger row is returned by two later checks
- **THEN** exactly one slot is linked to it and exactly one `PAIR_FUNDING_LINKED` event exists

#### Scenario: Ambiguous
- **WHEN** a ledger row falls inside two pairs' windows on the same symbol
- **THEN** it is linked to neither and the slot's last error says so

### Requirement: MANUAL_CHECK can still settle
A `MANUAL_CHECK` slot SHALL be re-checked by a "重新查詢" command and by an hourly sweep for 3 days after `t`. A found row SHALL move the slot to `SETTLED`.

#### Scenario: Late row
- **WHEN** a `MANUAL_CHECK` slot's row appears 2 hours after `t` and the sweep or the command runs
- **THEN** the slot is `SETTLED` and the PnL is recomputed

### Requirement: PnL follows the slots
For a pair with slots the PnL status SHALL be `COMPLETE` only when every slot is `SETTLED`; otherwise it SHALL be `INCOMPLETE` with a reason naming the tracing state (資費追蹤中 / 需人工檢查). Each transition to `SETTLED` SHALL recompute the PnL.

#### Scenario: One leg still tracing
- **WHEN** one leg is `SETTLED` and the other `TRACING_1`
- **THEN** the PnL is `INCOMPLETE` with reason 資費追蹤中

#### Scenario: Both settled
- **WHEN** both slots become `SETTLED`
- **THEN** a recomputed PnL is `COMPLETE` with the linked amounts

### Requirement: Backfill of earlier incomplete pairs
A one-time pass SHALL give finalized, non-simulated pairs whose PnL is `INCOMPLETE` with missing settlements slots in `MANUAL_CHECK`, due immediately, so that they are re-checked.

#### Scenario: NMR pair finalized without funding
- **WHEN** the pass runs for a pair finalized `INCOMPLETE` with 1 expected settlement per leg and 0 received
- **THEN** it has one slot per leg, re-checked at once, and `SETTLED` if the exchange has the row

### Requirement: Delay is measured
Every settled slot SHALL record `first_seen − t`, and the system SHALL be able to report p50/p95/p99 of it per exchange.

#### Scenario: Report
- **WHEN** 20 slots of an exchange are settled
- **THEN** the report lists the three percentiles for that exchange
