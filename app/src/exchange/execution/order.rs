//! Validated order inputs. A signed order request can only be built from a [`ValidOrder`], and a
//! `ValidOrder` cannot exist without a [`ClientOrderId`] (spec "沒有 client_order_id 的送單請求
//! SHALL 在型別層被拒絕"). The executor never generates or alters the id (design D3): it is the
//! engine's id, checked and passed through unchanged.

use tong_funding_core::types::Decimal;

use crate::engine::ids::{IdPrefix, MAX_LEN};
use crate::engine::ports::{OrderRequest, OrderSide};

/// A `client_order_id` accepted for a demo order: the engine's `demo` prefix, `[A-Za-z0-9_-]`,
/// 1..=36 characters (design D11; the exchanges' own limits are UNVERIFIED until task 4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientOrderId(String);

impl ClientOrderId {
    pub fn parse(id: &str) -> Result<ClientOrderId, String> {
        if id.is_empty() {
            return Err("client_order_id is required".into());
        }
        if id.len() > MAX_LEN {
            return Err(format!("client_order_id longer than {MAX_LEN} characters"));
        }
        if !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return Err("client_order_id may only contain [A-Za-z0-9_-]".into());
        }
        if IdPrefix::of(id) != Some(IdPrefix::Demo) {
            // A simulated (or foreign) id never reaches an exchange.
            return Err("demo executor only accepts demo-prefixed client_order_ids".into());
        }
        Ok(ClientOrderId(id.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Lookup key of one order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderRef {
    Client(ClientOrderId),
    /// The exchange's own order id.
    Exchange(String),
}

/// A market order the executor may send: everything an exchange needs, validated.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidOrder {
    id: ClientOrderId,
    symbol: String,
    side: OrderSide,
    quantity: Decimal,
    reduce_only: bool,
}

impl ValidOrder {
    pub fn from_request(req: &OrderRequest) -> Result<ValidOrder, String> {
        let id = ClientOrderId::parse(&req.client_order_id)?;
        if req.symbol.is_empty() || !req.symbol.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) {
            return Err(format!("invalid symbol {:?}", req.symbol));
        }
        if req.quantity <= Decimal::ZERO {
            return Err("quantity must be positive".into());
        }
        Ok(ValidOrder { id, symbol: req.symbol.clone(), side: req.side, quantity: req.quantity, reduce_only: req.reduce_only })
    }
    pub fn id(&self) -> &ClientOrderId {
        &self.id
    }
    pub fn symbol(&self) -> &str {
        &self.symbol
    }
    pub fn side(&self) -> OrderSide {
        self.side
    }
    /// The quantity exactly as core `Quantity` produced it (no re-formatting, no rounding here).
    pub fn quantity_text(&self) -> String {
        self.quantity.to_string()
    }
    pub fn reduce_only(&self) -> bool {
        self.reduce_only
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ids::client_order_id;
    use crate::engine::ports::{Leg, OrderAction};
    use tong_funding_core::types::Exchange;

    fn req(id: &str, qty: &str) -> OrderRequest {
        OrderRequest {
            client_order_id: id.into(),
            exchange: Exchange::Binance,
            symbol: "BTCUSDT".into(),
            side: OrderSide::Buy,
            quantity: qty.parse().unwrap(),
            reduce_only: false,
        }
    }

    #[test]
    fn an_order_without_a_client_order_id_cannot_be_built() {
        assert!(ValidOrder::from_request(&req("", "0.01")).is_err());
        assert!(ClientOrderId::parse("").is_err());
    }

    #[test]
    fn only_demo_ids_of_the_safe_charset_and_length_are_accepted() {
        let demo = client_order_id(IdPrefix::Demo, "ab12-uuid", Leg::Long, OrderAction::Open, 1);
        assert_eq!(ClientOrderId::parse(&demo).unwrap().as_str(), demo, "passed through unchanged");
        let sim = client_order_id(IdPrefix::Sim, "ab12-uuid", Leg::Long, OrderAction::Open, 1);
        assert!(ClientOrderId::parse(&sim).is_err(), "a simulated id never reaches an exchange");
        assert!(ClientOrderId::parse("demo_ab12_L_open_1").is_err(), "not an engine id");
        assert!(ClientOrderId::parse(&format!("{demo}x{}", "y".repeat(40))).is_err());
        assert!(ClientOrderId::parse("demo lo 1").is_err());
    }

    #[test]
    fn quantity_is_passed_through_as_core_produced_it() {
        let demo = client_order_id(IdPrefix::Demo, "u", Leg::Long, OrderAction::Open, 0);
        let o = ValidOrder::from_request(&req(&demo, "0.0190")).unwrap();
        assert_eq!(o.quantity_text(), "0.0190");
        assert!(ValidOrder::from_request(&req(&demo, "0")).is_err());
        assert!(ValidOrder::from_request(&req(&demo, "-1")).is_err());
    }
}
