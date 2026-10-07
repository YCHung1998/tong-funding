## ADDED Requirements

### Requirement: Opening orders carry the pair's entry leverage
The engine SHALL set `OrderRequest.leverage` to the `leverage` of the pair's entry snapshot on both opening legs of a pair. Close orders, reduce-only orders and manual-page orders SHALL carry no leverage.

#### Scenario: Both opening legs carry the same leverage
- **WHEN** a pair whose entry snapshot has leverage 5 sends its opening orders
- **THEN** the long and the short `OrderRequest` both have `leverage = Some(5)`

#### Scenario: Close orders carry none
- **WHEN** a pair sends its closing orders
- **THEN** every `OrderRequest` has `leverage = None`

### Requirement: Executor aligns exchange leverage before an opening order
For an order with `leverage = Some(n)` and `reduce_only = false`, the demo executor SHALL set the symbol's leverage to `n` on that exchange before sending the order (Binance `POST /fapi/v1/leverage`, Bybit `POST /v5/position/set-leverage` with equal buy and sell leverage). An answer meaning "already that leverage" SHALL count as success.

#### Scenario: Leverage set, then order sent
- **WHEN** an opening order with leverage 5 is submitted on either exchange
- **THEN** one leverage request with value 5 is sent first and the order request follows

#### Scenario: Already at that leverage
- **WHEN** Bybit answers retCode 110043 (leverage not modified)
- **THEN** the order is sent as normal

#### Scenario: No leverage on the request
- **WHEN** a request has `leverage = None` or `reduce_only = true`
- **THEN** no leverage request is sent

### Requirement: A failed leverage change blocks the order
If the leverage request is refused (other than "not modified"), rate-limited, answers unknown, cannot be built, or the leverage is not an integer in 1..=125, the executor SHALL NOT send the order and SHALL report a certain rejection (`not_sent`) with the reason.

#### Scenario: Refused leverage
- **WHEN** the exchange refuses the leverage request
- **THEN** no order request is sent and the outcome is `Rejected { code: "not_sent" }` naming the leverage failure

#### Scenario: Non-integer leverage
- **WHEN** the leverage is 2.5
- **THEN** no request of any kind is sent and the outcome is `not_sent`
