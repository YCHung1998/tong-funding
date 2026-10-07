## ADDED Requirements

### Requirement: Manual page has a leverage input
The manual order form SHALL have a Leverage field, initially the contract settings leverage and editable. For an opening order (not reduce-only) the value MUST be a whole number from 1 to 125, otherwise submit SHALL be disabled with a reason. For a reduce-only order the field SHALL be ignored.

#### Scenario: Invalid leverage
- **WHEN** the order is not reduce-only and the leverage is "2.5", "0", "126" or empty
- **THEN** submit is disabled and the reason names the leverage rule

#### Scenario: Reduce-only ignores leverage
- **WHEN** reduce-only is ticked and the leverage field is invalid
- **THEN** submit is not disabled because of the leverage

### Requirement: Manual page checks the exchange cap
For an opening order the page SHALL show the chosen exchange's leverage cap for the symbol at the order's estimated notional. A known cap below the leverage SHALL disable submit with "槓桿 N× 超過 <Exchange> 上限 C×". An unknown cap (failed, missing, older than 60 s, or read for a smaller notional) SHALL disable submit in EXCHANGE_DEMO and be informational in SIMULATION.

#### Scenario: Above the cap
- **WHEN** leverage is 5 and the exchange cap is 3
- **THEN** submit is disabled with "槓桿 5× 超過 Bybit 上限 3×"

#### Scenario: Reading for a larger notional is accepted
- **WHEN** the cap was read for a notional of 2000 and the order's estimate is 1500
- **THEN** that reading is used

#### Scenario: Reading for a smaller notional is not
- **WHEN** the cap was read for a notional of 500 and the estimate is 1500
- **THEN** the cap is unknown

### Requirement: The leverage is applied to the exchange
A confirmed opening manual order SHALL be sent to the engine with its leverage, and the engine SHALL put it on the order request so the executor sets it on the exchange before the order. A reduce-only order SHALL carry no leverage. A non-positive leverage SHALL be refused by the engine without sending anything.

#### Scenario: Opening order carries leverage
- **WHEN** a manual opening order with leverage 5 is confirmed
- **THEN** the `ManualOrder` command has `leverage = Some(5)` and the executor receives an `OrderRequest` with `leverage = Some(5)`

#### Scenario: Reduce-only carries none
- **WHEN** a reduce-only manual order is confirmed
- **THEN** the executor receives `leverage = None`

#### Scenario: Confirmation shows it
- **WHEN** the confirmation of an opening order opens
- **THEN** it shows the leverage and the cap check
