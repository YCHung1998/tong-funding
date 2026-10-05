//! Single-symbol re-fetch support (task 2.4): the earliest-receive-time rule, plus the cross-exchange
//! tests that prove every re-fetch sends fresh requests, reads no cache and uses the single-request
//! timeout. The per-exchange request logic lives next to each adapter (`refetch_symbol`).
#![allow(dead_code)]

/// `observed_at` of a re-fetch that needed several requests: the earliest receive time, because the
/// oldest piece of data decides how fresh the whole observation is. `None` for an empty list.
pub(crate) fn earliest_observed_at(received_at: &[i64]) -> Option<i64> {
    received_at.iter().copied().min()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earliest_of_several_receive_times() {
        assert_eq!(earliest_observed_at(&[1_300, 1_000, 1_100]), Some(1_000));
        assert_eq!(earliest_observed_at(&[5]), Some(5));
        assert_eq!(earliest_observed_at(&[]), None);
    }

    // ----- cross-exchange behaviour of `refetch_symbol` (spec: 單一標的的重新抓取不得使用任何快取) -----

    use std::sync::Arc;
    use std::time::Duration;

    use super::super::adapter::testkit::{ClockedTransport, block_on, fixture};
    use super::super::adapter::{ExchangeAdapter, SINGLE_TIMEOUT};
    use super::super::binance::BinanceAdapter;
    use super::super::bybit::BybitAdapter;
    use super::super::okx::OkxAdapter;
    use crate::exchange::error::AdapterError;
    use crate::exchange::transport::{FakeTransport, HttpResponse};
    use crate::ports::ManualClock;

    const NOW: i64 = 1_791_201_300_000;

    fn ok(rel: &str) -> Result<HttpResponse, AdapterError> {
        Ok(HttpResponse::ok(fixture(rel)))
    }

    /// Everything each exchange needs for one batch snapshot AND one single-symbol re-fetch of BTC.
    fn binance_fake() -> FakeTransport {
        FakeTransport::new()
            .on("premiumIndex?symbol=BTCUSDT", ok("binance/premiumIndex_single_btcusdt.json"))
            .on("ticker/24hr?symbol=BTCUSDT", ok("binance/ticker24hr_single_btcusdt.json"))
            .on("/fapi/v1/premiumIndex", ok("binance/premiumIndex.json"))
            .on("/fapi/v1/ticker/24hr", ok("binance/ticker24hr.json"))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"))
    }

    fn bybit_fake() -> FakeTransport {
        FakeTransport::new()
            .on("tickers?category=linear&symbol=BTCUSDT", ok("bybit/tickers_single_btcusdt.json"))
            .on("/v5/market/tickers", ok("bybit/tickers.json"))
            .on("/v5/market/instruments-info", ok("bybit/instruments_complete.json"))
    }

    fn okx_fake() -> FakeTransport {
        FakeTransport::new()
            .on("funding-rate?instId=BTC-USDT-SWAP", ok("okx/funding_rate_btc.json"))
            .on("mark-price?instId=BTC-USDT-SWAP", ok("okx/mark_price_btc.json"))
            .on("market/ticker?instId=BTC-USDT-SWAP", ok("okx/ticker_btc.json"))
            .on("funding-rate?instId=ANY", ok("okx/funding_rate_any.json"))
            .on("mark-price?instType=SWAP", ok("okx/mark_price_swap.json"))
            .on("market/tickers?instType=SWAP", ok("okx/tickers_swap.json"))
            .on("public/instruments?instType=SWAP", ok("okx/instruments_swap.json"))
    }

    /// Runs the shared scenarios against one adapter; `single_markers` identify its single-symbol requests.
    fn check<A: ExchangeAdapter>(
        label: &str,
        make: impl Fn(Arc<ClockedTransport>, ManualClock) -> A,
        fake: impl Fn() -> FakeTransport,
        single_markers: &[&str],
    ) {
        // 1. warm batch data exists; every re-fetch still sends one new request per marker
        let clock = ManualClock::new(NOW);
        let transport = Arc::new(ClockedTransport::new(fake(), clock.clone(), 0));
        let adapter = make(Arc::clone(&transport), clock.clone());
        let batch = block_on(adapter.fetch_snapshot()).unwrap();
        let cached_at = batch.iter().find(|o| o.symbol == "BTCUSDT").unwrap().observed_at;
        clock.advance(8_000);
        let first = block_on(adapter.refetch_symbol("BTCUSDT")).unwrap();
        clock.advance(8_000);
        let second = block_on(adapter.refetch_symbol("BTCUSDT")).unwrap();
        for m in single_markers {
            assert_eq!(transport.count(m), 2, "{label}: {m} must be requested on every re-fetch");
        }
        assert!(first.observed_at > cached_at, "{label}");
        assert!(second.observed_at > first.observed_at, "{label}");
        for r in transport.requests().iter().filter(|r| single_markers.iter().any(|m| r.url.contains(m))) {
            assert_eq!(r.timeout, SINGLE_TIMEOUT, "{label}");
            assert_eq!(r.timeout, Duration::from_secs(2), "{label}");
        }

        // 2. requests answered 300 ms apart: observed_at is the earliest receive time
        let clock = ManualClock::new(NOW);
        let transport = Arc::new(ClockedTransport::new(fake(), clock.clone(), 300));
        let adapter = make(transport, clock);
        let o = block_on(adapter.refetch_symbol("BTCUSDT")).unwrap();
        assert_eq!(o.observed_at, NOW, "{label}: earliest of the requests, not the last");
    }

    #[test]
    fn binance_refetch_is_uncached_timed_and_takes_the_earliest_time() {
        check("binance", |t, c| BinanceAdapter::new(t, Arc::new(c)), binance_fake, &["premiumIndex?symbol=BTCUSDT", "ticker/24hr?symbol=BTCUSDT"]);
    }

    #[test]
    fn bybit_refetch_is_uncached_timed_and_takes_the_earliest_time() {
        check("bybit", |t, c| BybitAdapter::new(t, Arc::new(c)), bybit_fake, &["tickers?category=linear&symbol=BTCUSDT"]);
    }

    #[test]
    fn okx_refetch_is_uncached_timed_and_takes_the_earliest_time() {
        check(
            "okx",
            |t, c| OkxAdapter::new(t, Arc::new(c)),
            okx_fake,
            &["funding-rate?instId=BTC-USDT-SWAP", "mark-price?instId=BTC-USDT-SWAP", "market/ticker?instId=BTC-USDT-SWAP"],
        );
    }

    #[test]
    fn all_three_adapters_return_the_same_observation_type_with_consistent_units() {
        // spec: 三所輸出同一種型別 — rate is a fraction, price and volume are Decimal, status Listed
        use tong_funding_core::funding::{DataStatus, FundingObservation};
        let clock = ManualClock::new(NOW);
        let mk = |fake: FakeTransport| Arc::new(ClockedTransport::new(fake, clock.clone(), 0));
        let b: FundingObservation = block_on(BinanceAdapter::new(mk(binance_fake()), Arc::new(clock.clone())).refetch_symbol("BTCUSDT")).unwrap();
        let y: FundingObservation = block_on(BybitAdapter::new(mk(bybit_fake()), Arc::new(clock.clone())).refetch_symbol("BTCUSDT")).unwrap();
        let o: FundingObservation = block_on(OkxAdapter::new(mk(okx_fake()), Arc::new(clock.clone())).refetch_symbol("BTCUSDT")).unwrap();
        for obs in [&b, &y, &o] {
            assert_eq!(obs.symbol, "BTCUSDT");
            assert_eq!(obs.data_status, DataStatus::Listed);
            assert!(obs.funding_rate.abs() < tong_funding_core::types::Decimal::new(1, 2), "rate is a fraction (< 1%): {}", obs.funding_rate);
            assert!(obs.mark_price > tong_funding_core::types::Decimal::new(10_000, 0));
            assert!(obs.volume_24h_quote.is_some_and(|v| v > tong_funding_core::types::Decimal::new(1_000_000_000, 0)), "volume in USDT: {:?}", obs.volume_24h_quote);
            assert_eq!(obs.funding_interval_secs, Some(28_800));
        }
    }
}
