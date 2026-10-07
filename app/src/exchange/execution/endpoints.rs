//! Order endpoint paths (hosts come from `signed::endpoints`, demo/testnet only). Parameter names
//! and paths follow the exchanges' public v1 / v5 documentation as I remember it: UNVERIFIED
//! against a real demo account until task 4.2 (design Open Questions 2).

/// Binance USDS-M Futures: new order (POST), query order (GET), cancel order (DELETE).
pub const BINANCE_ORDER_PATH: &str = "/fapi/v1/order";
/// Binance: `{"dualSidePosition": false}` = one-way mode.
pub const BINANCE_POSITION_MODE_PATH: &str = "/fapi/v1/positionSide/dual";
/// Binance: fills of one order (commission per trade), for the fee of a filled order.
pub const BINANCE_USER_TRADES_PATH: &str = "/fapi/v1/userTrades";

/// Bybit v5: create order (POST, JSON body).
pub const BYBIT_CREATE_PATH: &str = "/v5/order/create";
/// Bybit v5: cancel order (POST, JSON body).
pub const BYBIT_CANCEL_PATH: &str = "/v5/order/cancel";
/// Bybit v5: open and recent orders, queried by `orderLinkId` / `orderId`.
pub const BYBIT_REALTIME_PATH: &str = "/v5/order/realtime";
/// Bybit v5: order history (fallback when an order is no longer in `realtime`).
pub const BYBIT_HISTORY_PATH: &str = "/v5/order/history";
/// Bybit v5: position list of one symbol (`positionIdx` 0 = one-way).
pub const BYBIT_POSITION_PATH: &str = "/v5/position/list";

/// Binance response type that carries `executedQty` / `avgPrice` in the order ACK (UNVERIFIED
/// whether the testnet honours it for MARKET orders).
pub const BINANCE_RESP_TYPE: &str = "RESULT";

/// OKX v5: place order (POST) and order details (GET) share one path; cancel is its own path.
pub const OKX_ORDER_PATH: &str = "/api/v5/trade/order";
pub const OKX_CANCEL_PATH: &str = "/api/v5/trade/cancel-order";
/// OKX v5: account configuration (`acctLv`, `posMode`) for the one-way gate.
pub const OKX_ACCOUNT_CONFIG_PATH: &str = "/api/v5/account/config";
