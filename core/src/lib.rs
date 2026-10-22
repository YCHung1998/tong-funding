//! Pure domain logic. No I/O, no GPUI, no system clock (time is always passed in).

pub mod edge_kind;
pub mod cost_attribution;
pub mod funding;
pub mod grouping;
pub mod net_edge;
pub mod pair;
pub mod pnl;
pub mod pretrade;
pub mod quantity;
pub mod redact;
pub mod risk;
pub mod spread_tag;
pub mod trade_cost;
pub mod types;
