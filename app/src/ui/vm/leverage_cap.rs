//! symbol-leverage-cap view-model rule: does the contract leverage fit both legs' exchange caps?
//! Pure. A reading is only evidence while it is fresh and was taken for the same notional.

use tong_funding_core::risk::ExecutionMode;
use tong_funding_core::types::{Decimal, Exchange};

use super::bridge::{CapReading, UiSnapshot};

/// A cap reading older than this is "unknown" (the loader refreshes every ~20 s).
pub const CAP_FRESH_MS: i64 = 60_000;

/// The cap of one leg as the page shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum LegCap {
    Known(Decimal),
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CapCheck {
    pub long: (Exchange, LegCap),
    pub short: (Exchange, LegCap),
    pub leverage: Decimal,
}

/// What the check means for the pair.
#[derive(Debug, Clone, PartialEq)]
pub enum CapVerdict {
    /// Both caps known and at least the leverage.
    Complies,
    /// A known cap is below the leverage (wins over an unknown other leg).
    Exceeds { exchange: Exchange, cap: Decimal },
    /// No known cap is below the leverage but at least one is unknown.
    Unknown { exchange: Exchange, why: String },
}

fn leg_cap(snap: &UiSnapshot, exchange: Exchange, symbol: &str, notional: Decimal, now_ms: i64) -> LegCap {
    match snap.leverage_caps.get(&(exchange, symbol.to_string())) {
        None => LegCap::Unknown("尚未查詢".into()),
        Some(CapReading { cap: Err(e), .. }) => LegCap::Unknown(e.clone()),
        // A larger notional never has a larger cap (Binance brackets), so a reading taken for an
        // equal or larger notional is safe evidence; a smaller one is not.
        Some(CapReading { notional: n, .. }) if *n < notional => LegCap::Unknown("名目金額已變更，等待重新查詢".into()),
        Some(CapReading { fetched_at, .. }) if now_ms.saturating_sub(*fetched_at) > CAP_FRESH_MS => LegCap::Unknown("查詢已過期".into()),
        Some(CapReading { cap: Ok(c), .. }) => LegCap::Known(*c),
    }
}

/// One leg's cap (manual orders).
pub fn leg(snap: &UiSnapshot, exchange: Exchange, symbol: &str, notional: Decimal, now_ms: i64) -> LegCap {
    leg_cap(snap, exchange, symbol, notional, now_ms)
}

/// Why a single-leg opening order with `leverage` cannot be sent, if it cannot (same rules as pairs).
pub fn leg_blocked_reason(exchange: Exchange, cap: &LegCap, leverage: Decimal, mode: Option<ExecutionMode>) -> Option<String> {
    match cap {
        LegCap::Known(c) if *c < leverage => Some(format!("槓桿 {}× 超過 {} 上限 {}×", leverage.normalize(), exchange.name(), c.normalize())),
        LegCap::Known(_) => None,
        LegCap::Unknown(why) => (mode == Some(ExecutionMode::ExchangeDemo)).then(|| format!("{} 槓桿上限未知（{why}）", exchange.name())),
    }
}

/// One line for the page, e.g. `Bybit 上限 3× ✗`.
pub fn leg_text(exchange: Exchange, cap: &LegCap, leverage: Decimal) -> String {
    match cap {
        LegCap::Known(c) if *c < leverage => format!("{} 上限 {}× ✗ 槓桿 {}× 超過上限", exchange.name(), c.normalize(), leverage.normalize()),
        LegCap::Known(c) => format!("{} 上限 {}× ✓", exchange.name(), c.normalize()),
        LegCap::Unknown(why) => format!("{} 上限未知（{why}）", exchange.name()),
    }
}

pub fn check(snap: &UiSnapshot, long: Exchange, short: Exchange, symbol: &str, notional: Decimal, leverage: Decimal, now_ms: i64) -> CapCheck {
    CapCheck {
        long: (long, leg_cap(snap, long, symbol, notional, now_ms)),
        short: (short, leg_cap(snap, short, symbol, notional, now_ms)),
        leverage,
    }
}

impl CapCheck {
    pub fn verdict(&self) -> CapVerdict {
        let legs = [&self.long, &self.short];
        if let Some((exchange, LegCap::Known(cap))) = legs.iter().find(|(_, c)| matches!(c, LegCap::Known(c) if *c < self.leverage)) {
            return CapVerdict::Exceeds { exchange: *exchange, cap: *cap };
        }
        if let Some((exchange, LegCap::Unknown(why))) = legs.iter().find(|(_, c)| matches!(c, LegCap::Unknown(_))) {
            return CapVerdict::Unknown { exchange: *exchange, why: why.clone() };
        }
        CapVerdict::Complies
    }

    /// Why the pair cannot be selected / added, if it cannot. `Unknown` blocks only in EXCHANGE_DEMO.
    pub fn blocked_reason(&self, mode: Option<ExecutionMode>) -> Option<String> {
        match self.verdict() {
            CapVerdict::Complies => None,
            CapVerdict::Exceeds { exchange, cap } => Some(format!("槓桿 {}× 超過 {} 上限 {}×", self.leverage.normalize(), exchange.name(), cap.normalize())),
            CapVerdict::Unknown { exchange, why } => {
                (mode == Some(ExecutionMode::ExchangeDemo)).then(|| format!("{} 槓桿上限未知（{why}）", exchange.name()))
            }
        }
    }

    /// One line for the page, e.g. `上限 Binance 20× · Bybit 3×（槓桿 5× 超過 Bybit 上限）`.
    pub fn text(&self) -> String {
        let leg = |(e, c): &(Exchange, LegCap)| match c {
            LegCap::Known(c) => format!("{} {}×", e.name(), c.normalize()),
            LegCap::Unknown(why) => format!("{} 未知（{why}）", e.name()),
        };
        let base = format!("上限 {} · {}", leg(&self.long), leg(&self.short));
        match self.verdict() {
            CapVerdict::Complies => format!("{base} ✓"),
            CapVerdict::Exceeds { exchange, .. } => format!("{base} ✗ 槓桿 {}× 超過 {} 上限", self.leverage.normalize(), exchange.name()),
            CapVerdict::Unknown { .. } => base,
        }
    }
}

#[cfg(test)]
#[path = "leverage_cap_tests.rs"]
mod tests;
