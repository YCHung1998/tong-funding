//! `DemoAccountView`: the engine's read-only `AccountView` over the existing signed GET clients
//! (exchange-readonly-adapters). Positions are signed (long positive, short negative): Binance
//! `positionAmt` is already signed, Bybit's unsigned `size` is normalised with `side` by the
//! signed client (design Open Question 8). Lists say whether they are complete; an incomplete
//! list is never "nothing". A hedge-mode position makes the list unusable (`Err`): the engine
//! assumes one-way and summing both sides would hide exposure. OKX (okx-signed-read): positions in CONTRACTS (`AccountPosition` unit contract), open orders
//! `sz - accFillSz` in contracts, margin = the exchange's `availEq`. Without a wired OKX client, a
//! missing key, an unsupported account mode or a failed query is an `Err` with the reason.

use std::sync::Arc;

use tong_funding_core::types::{Decimal, Exchange};

use crate::engine::ports::{AccountOrder, AccountPosition, AccountView, BoxFut, Listed};
use crate::exchange::signed::binance::BinanceSignedClient;
use crate::exchange::signed::bybit::BybitSignedClient;
use crate::exchange::signed::okx::OkxSignedClient;
use crate::exchange::error::AdapterError;
use crate::exchange::signed::models::{Balance, Completeness, OpenOrder, Position, PositionMode};
use crate::exchange::transport::HttpTransport;

/// Reason when no OKX client was given to the view (not wired yet: UI wiring is okx-trading-enablement).
pub const OKX_NOT_WIRED: &str = "OKX account client is not wired";
/// Asset whose available balance is the margin of USDT-margined contracts.
pub const MARGIN_ASSET: &str = "USDT";

pub struct DemoAccountView<T> {
    binance: Arc<BinanceSignedClient<T>>,
    bybit: Arc<BybitSignedClient<T>>,
    okx: Option<Arc<OkxSignedClient<T>>>,
}

impl<T: HttpTransport + 'static> DemoAccountView<T> {
    pub fn new(binance: Arc<BinanceSignedClient<T>>, bybit: Arc<BybitSignedClient<T>>) -> Self {
        DemoAccountView { binance, bybit, okx: None }
    }

    /// Adds the OKX signed client; without it every OKX query is `Err(OKX_NOT_WIRED)`.
    pub fn with_okx(mut self, okx: Arc<OkxSignedClient<T>>) -> Self {
        self.okx = Some(okx);
        self
    }

    /// Error text of an OKX query; a not-connected answer names its reason (`NoPassphrase`, ...).
    fn okx_error(okx: &OkxSignedClient<T>, e: AdapterError) -> String {
        match (&e, okx.last_not_connected_reason()) {
            (AdapterError::NotConnected, Some(reason)) => format!("OKX: not connected ({reason:?})"),
            _ => format!("OKX: {e}"),
        }
    }

    fn okx(&self) -> Result<&Arc<OkxSignedClient<T>>, String> {
        self.okx.as_ref().ok_or_else(|| OKX_NOT_WIRED.to_string())
    }
}

/// Engine positions from adapter positions; hedge mode is refused.
pub fn to_positions(exchange: Exchange, rows: Vec<Position>, complete: bool) -> Result<Listed<AccountPosition>, String> {
    if let Some(p) = rows.iter().find(|p| p.mode == PositionMode::Hedge) {
        return Err(format!("{}: hedge-mode position on {}; the system assumes one-way mode", exchange.name(), p.symbol));
    }
    let items = rows
        .into_iter()
        .filter(|p| !p.quantity.is_zero())
        .map(|p| AccountPosition { exchange, symbol: p.symbol, quantity: p.quantity })
        .collect();
    Ok(Listed { items, complete })
}

/// Engine open orders. `client_order_id` is not in the adapter's model: `None` (the flat check
/// and the foreign-exposure check only need the symbol).
pub fn to_orders(exchange: Exchange, rows: Vec<OpenOrder>, complete: bool) -> Listed<AccountOrder> {
    let items = rows
        .into_iter()
        .map(|o| AccountOrder { exchange, symbol: o.symbol, client_order_id: None, remaining_quantity: (o.quantity - o.filled_quantity).max(Decimal::ZERO) })
        .collect();
    Listed { items, complete }
}

/// Available USDT; missing = not available (the Margin check then fails closed).
pub fn to_margin(exchange: Exchange, balances: &[Balance]) -> Result<Decimal, String> {
    balances
        .iter()
        .find(|b| b.asset == MARGIN_ASSET)
        .and_then(|b| b.available)
        .ok_or_else(|| format!("{}: available {MARGIN_ASSET} balance not reported", exchange.name()))
}

impl<T: HttpTransport + 'static> AccountView for DemoAccountView<T> {
    fn positions(&self, exchange: Exchange) -> BoxFut<'_, Result<Listed<AccountPosition>, String>> {
        Box::pin(async move {
            match exchange {
                Exchange::Binance => {
                    let rows = self.binance.get_positions().await.map_err(|e| e.to_string())?;
                    to_positions(exchange, rows, true)
                }
                Exchange::Bybit => {
                    let l = self.bybit.get_positions().await.map_err(|e| e.to_string())?;
                    let complete = l.is_complete();
                    to_positions(exchange, l.items, complete)
                }
                Exchange::Okx => {
                    let okx = self.okx()?;
                    let l = okx.get_positions().await.map_err(|e| Self::okx_error(okx, e))?;
                    let items = l.items.into_iter().map(|p| AccountPosition { exchange, symbol: p.symbol, quantity: p.contracts }).collect();
                    Ok(Listed { items, complete: l.completeness == Completeness::Complete })
                }
            }
        })
    }

    fn open_orders(&self, exchange: Exchange) -> BoxFut<'_, Result<Listed<AccountOrder>, String>> {
        Box::pin(async move {
            match exchange {
                Exchange::Binance => {
                    let rows = self.binance.get_open_orders().await.map_err(|e| e.to_string())?;
                    Ok(to_orders(exchange, rows, true))
                }
                Exchange::Bybit => {
                    let l = self.bybit.get_open_orders().await.map_err(|e| e.to_string())?;
                    let complete = l.is_complete();
                    Ok(to_orders(exchange, l.items, complete))
                }
                Exchange::Okx => {
                    let okx = self.okx()?;
                    let l = okx.get_open_orders().await.map_err(|e| Self::okx_error(okx, e))?;
                    let complete = l.is_complete();
                    Ok(to_orders(exchange, l.items, complete))
                }
            }
        })
    }

    fn available_margin(&self, exchange: Exchange) -> BoxFut<'_, Result<Decimal, String>> {
        Box::pin(async move {
            match exchange {
                Exchange::Binance => to_margin(exchange, &self.binance.get_balances().await.map_err(|e| e.to_string())?),
                Exchange::Bybit => self.bybit.get_available_margin().await.map_err(|e| format!("Bybit: {e}")),
                Exchange::Okx => {
                    let okx = self.okx()?;
                    okx.get_available_margin().await.map_err(|e| Self::okx_error(okx, e))
                }
            }
        })
    }
}
