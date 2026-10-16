//! The real [`RateSource`]: the exchanges' public funding-rate history through the signed GET
//! clients of the demo hosts (same environment as the account whose funding is traced). An
//! exchange without a client answers `Err`, which the slot machine treats as "rate unknown".

use std::sync::Arc;

use tong_funding_core::types::{Decimal, Exchange};

use super::fetch::BoxFut;
use super::slot_runner::RateSource;
use crate::exchange::signed::binance::BinanceSignedClient;
use crate::exchange::signed::endpoints::{BinanceEnv, BinanceHost, BybitEnv, BybitHost};
use crate::exchange::signed::bybit::BybitSignedClient;
use crate::exchange::transport::HttpTransport;

pub struct DemoRateSource<T, BE = BinanceHost, YE = BybitHost> {
    pub binance: Arc<BinanceSignedClient<T, BE>>,
    pub bybit: Arc<BybitSignedClient<T, YE>>,
}

impl<T: HttpTransport + 'static, BE: BinanceEnv, YE: BybitEnv> RateSource for DemoRateSource<T, BE, YE> {
    fn rate_at<'a>(&'a self, exchange: Exchange, symbol: &'a str, settle_ms: i64) -> BoxFut<'a, Result<Option<Decimal>, String>> {
        Box::pin(async move {
            match exchange {
                Exchange::Binance => self.binance.get_funding_rate_at(symbol, settle_ms).await.map_err(|e| format!("Binance: {e}")),
                Exchange::Bybit => self.bybit.get_funding_rate_at(symbol, settle_ms).await.map_err(|e| format!("Bybit: {e}")),
                Exchange::Okx => Err("OKX funding-rate history is not wired in this build".to_string()),
            }
        })
    }
}
