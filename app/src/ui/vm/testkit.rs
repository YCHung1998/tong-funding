//! Builders shared by the view-model tests.

use tong_funding_core::funding::{DataStatus, FundingObservation};
use tong_funding_core::pair::PairState;
use tong_funding_core::risk::RiskConfig;
use tong_funding_core::types::{Decimal, Exchange, Side};

use super::bridge::{AccountData, AccountState, AssetInput, PairInfo, Settings};
use crate::exchange::signed::models::{Position, PositionMode};

pub fn d(s: &str) -> Decimal {
    s.parse().unwrap_or_else(|_| panic!("bad decimal {s}"))
}

/// A `Listed` observation (rate is a fraction) with plenty of volume.
pub fn obs(ex: Exchange, symbol: &str, rate: &str, interval_secs: i64, next_funding_time: i64, observed_at: i64) -> FundingObservation {
    FundingObservation::new(ex, symbol, d(rate), Some(interval_secs), next_funding_time, d("100"), Some(d("100000000")), observed_at, observed_at, DataStatus::Listed)
}

pub fn with_status(mut o: FundingObservation, status: DataStatus) -> FundingObservation {
    o.data_status = status;
    o
}

/// Complete settings: fees 0.02 on every exchange, slippage/safety 0, threshold `threshold`.
pub fn complete_settings(threshold: &str) -> Settings {
    let mut risk = RiskConfig::default();
    risk.net_edge_threshold_pct = Some(d(threshold));
    risk.est_slippage_pct = Some(d("0"));
    risk.safety_margin_pct = d("0");
    risk.min_24h_volume_usdt = d("0");
    // ui-trading-pages added min_expected_net_pnl_pct (default 0.03); these fixtures test the
    // Net Edge threshold alone, so the second threshold is off here (tests that need it set it).
    risk.min_expected_net_pnl_pct = d("0");
    for e in Exchange::ALL {
        risk.taker_fee_pct.insert(e, d("0.02"));
    }
    Settings { risk, ..Settings::default() }
}

#[allow(clippy::too_many_arguments)]
pub fn position(ex: Exchange, symbol: &str, qty: &str, entry: &str, mark: &str, lev: &str, pnl: &str, margin: Option<&str>) -> Position {
    let q = d(qty);
    Position {
        exchange: ex,
        symbol: symbol.into(),
        side: if q.is_sign_negative() { Side::Short } else { Side::Long },
        quantity: q,
        entry_price: Some(d(entry)),
        mark_price: Some(d(mark)),
        leverage: Some(d(lev)),
        unrealized_pnl: Some(d(pnl)),
        margin: margin.map(d),
        notional: Some((q * d(mark)).abs()),
        mode: PositionMode::OneWay,
        fetched_at: 0,
    }
}

pub fn asset(name: &str, qty: &str, usdt_value: Option<&str>, mark: Option<&str>) -> AssetInput {
    AssetInput { asset: name.into(), quantity: d(qty), usdt_value: usdt_value.map(d), mark_price: mark.map(d) }
}

pub fn loaded(assets: Vec<AssetInput>, positions: Vec<Position>, fetched_at: i64) -> AccountState {
    AccountState::Loaded {
        data: AccountData { assets, contract_equity: None, positions, positions_incomplete: None, environment: "DEMO" },
        fetched_at,
        error: None,
    }
}

pub fn pair(id: &str, symbol: &str, long: Exchange, short: Exchange, state: PairState) -> PairInfo {
    PairInfo { pair_id: id.into(), symbol: symbol.into(), long_exchange: long, short_exchange: short, state: Ok(state) }
}
