## ADDED Requirements

### Requirement: Per-symbol leverage cap lookup
The system SHALL read the maximum leverage an exchange allows for a symbol at a given position notional: for Binance the `initialLeverage` of the `leverageBracket` whose `notionalFloor <= notional <= notionalCap`, for Bybit the symbol's `leverageFilter.maxLeverage`. A notional above the last Binance bracket, a missing field, or a non-trading Bybit symbol SHALL be an error, never a default.

#### Scenario: Binance bracket by notional
- **WHEN** the brackets are 1:125 up to 50 000, 2:100 up to 250 000 and the notional is 100 000
- **THEN** the cap is 100

#### Scenario: Bybit symbol cap
- **WHEN** `instruments-info` reports `maxLeverage` "50.00"
- **THEN** the cap is 50

#### Scenario: Notional beyond every bracket
- **WHEN** the notional is above the last bracket's cap
- **THEN** the lookup fails with a reason

### Requirement: Staged orders show and verify the cap
Each PREPARED pair on the staged-orders page SHALL show both legs' cap and whether its leverage complies. A pair whose leverage is above either leg's cap SHALL NOT be selectable for one-click submit, with a reason naming the exchange and the cap. When a cap is unknown (failed, missing, older than 60 s, or read for another notional) the pair SHALL NOT be selectable in EXCHANGE_DEMO; in SIMULATION it SHALL be selectable and the cap shown as unknown.

#### Scenario: Leverage above a cap
- **WHEN** a pair's leverage is 5, Bybit's cap is 3 and Binance's is 20
- **THEN** the row shows both caps, is not selectable, and says "槓桿 5× 超過 Bybit 上限 3×"

#### Scenario: Compliant leverage
- **WHEN** leverage is 5 and both caps are at least 5
- **THEN** the row shows both caps and stays selectable (other rules permitting)

#### Scenario: Unknown cap in EXCHANGE_DEMO
- **WHEN** a cap read failed and the mode is EXCHANGE_DEMO
- **THEN** the row is not selectable and says the cap is unknown

#### Scenario: Unknown cap in SIMULATION
- **WHEN** a cap read failed and the mode is SIMULATION
- **THEN** the row stays selectable and shows the cap as unknown

### Requirement: Candidate list verifies the cap before adding
The scanner candidate list SHALL show the cap check of each ticked symbol and SHALL NOT add a symbol to the staged orders when a known cap of either leg is below the contract leverage. An unknown cap SHALL only be shown.

#### Scenario: Known cap below leverage
- **WHEN** a ticked symbol's known cap is below the contract leverage
- **THEN** "add to staged orders" does not send it and the list says why

### Requirement: Engine blocks a pair above the cap before sending anything
Before the opening orders of an EXCHANGE_DEMO pair, the engine SHALL read both legs' caps fresh for the pair's notional. If the leverage is above either cap, or a cap cannot be read, the pair SHALL be BLOCKED with the check `LeverageCap` and no leverage or order request SHALL be sent. SIMULATION pairs SHALL skip this gate.

#### Scenario: Above the cap
- **WHEN** an EXCHANGE_DEMO pair has leverage 5 and a leg's fresh cap is 3
- **THEN** the pair becomes BLOCKED with `LeverageCap` among its failed checks and nothing is sent

#### Scenario: Cap unreadable
- **WHEN** a leg's cap read fails
- **THEN** the pair is BLOCKED (fail closed) and nothing is sent

#### Scenario: Within the caps
- **WHEN** both fresh caps are at least the leverage
- **THEN** the pair proceeds to its orders

#### Scenario: Simulation pair
- **WHEN** a SIMULATION pair enters
- **THEN** no cap is read and the gate does not block
