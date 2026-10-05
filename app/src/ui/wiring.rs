//! Composition-root adapters (ui-trading-pages A): the real engine ports built from the pieces the
//! data source already runs. Only `live.rs` uses this file; pages never see it.
//!
//! - [`PublicMarketData`]: `MarketData` over the three public adapters — single-symbol
//!   `refetch_symbol` (never a cache) and market-order lot rules (`MARKET_LOT_SIZE` falling back to
//!   `LOT_SIZE`; OKX in contracts with `ctVal`, required).
//! - [`ClockOffsets`]: `ServerOffsets` from the existing `ClockSync`s (unsynced = `None`: no entry).
//! - [`feed_prices`]: market observations → `SimPriceBook` (simulated fills) and the engine's
//!   market `watch` (snapshot prices).
//! - [`NoAccount`]: the account view when the signed transport could not be built (every read
//!   `Err`, so margins fail closed).

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::watch;
use tong_funding_core::funding::{DataStatus, FundingObservation};
use tong_funding_core::types::{Decimal, Exchange};

use crate::engine::actor::MarketPrices;
use crate::engine::ports::{AccountOrder, AccountPosition, AccountView, BoxFut, FreshQuote, Listed, MarketData, OrderRules, ServerOffsets};
use crate::engine::sim::SimPriceBook;
use crate::exchange::health::clock_sync::{ClockStatus, ClockSync};
use crate::exchange::public::adapter::{ExchangeAdapter, RulesLookup};

/// A re-fetched observation as the engine's fresh quote: price = mark price (the scan price is the
/// mark price too, so drift compares like with like), observed at the receive time.
pub fn fresh_quote(o: FundingObservation) -> FreshQuote {
    FreshQuote { price: o.mark_price, price_observed_at_ms: o.observed_at, listed: o.data_status == DataStatus::Listed, funding: o }
}

/// Instrument rules → the engine's market-order rules. Never a default step.
pub fn order_rules_of(exchange: Exchange, lookup: RulesLookup) -> Result<OrderRules, String> {
    match lookup {
        RulesLookup::Available(r) => {
            if exchange == Exchange::Okx && r.ct_val.is_none() {
                return Err("OKX ctVal missing".into());
            }
            Ok(OrderRules { lot: r.market_lot_size(), okx_ct_val: r.ct_val })
        }
        RulesLookup::Unavailable { reason } => Err(reason),
        RulesLookup::UnknownSymbol => Err("symbol not in the exchange catalog".into()),
    }
}

/// `MarketData` over the three public adapters.
pub struct PublicMarketData<B, Y, O> {
    pub adapters: Arc<(B, Y, O)>,
}

impl<B: ExchangeAdapter + 'static, Y: ExchangeAdapter + 'static, O: ExchangeAdapter + 'static> MarketData for PublicMarketData<B, Y, O> {
    fn refetch(&self, exchange: Exchange, symbol: &str) -> BoxFut<'_, Result<FreshQuote, String>> {
        let symbol = symbol.to_string();
        Box::pin(async move {
            let r = match exchange {
                Exchange::Binance => self.adapters.0.refetch_symbol(&symbol).await,
                Exchange::Bybit => self.adapters.1.refetch_symbol(&symbol).await,
                Exchange::Okx => self.adapters.2.refetch_symbol(&symbol).await,
            };
            r.map(fresh_quote).map_err(|e| e.to_string())
        })
    }

    fn order_rules(&self, exchange: Exchange, symbol: &str) -> BoxFut<'_, Result<OrderRules, String>> {
        let symbol = symbol.to_string();
        Box::pin(async move {
            let r = match exchange {
                Exchange::Binance => self.adapters.0.instrument_rules(&symbol).await,
                Exchange::Bybit => self.adapters.1.instrument_rules(&symbol).await,
                Exchange::Okx => self.adapters.2.instrument_rules(&symbol).await,
            };
            r.map_err(|e| e.to_string()).and_then(|l| order_rules_of(exchange, l))
        })
    }
}

/// `ServerOffsets` from the clock syncs the data source keeps calibrated.
pub struct ClockOffsets {
    pub clocks: BTreeMap<Exchange, Arc<ClockSync>>,
}

impl ServerOffsets for ClockOffsets {
    fn offset_ms(&self, exchange: Exchange) -> Option<i64> {
        match self.clocks.get(&exchange)?.status() {
            ClockStatus::Synced { offset_ms, .. } => Some(offset_ms),
            ClockStatus::Unsynced => None,
        }
    }
}

/// Feeds every listed observation's mark price to the simulator's price book and the engine's
/// market `watch` (one `send_modify`, so the actor sees one change per batch).
pub fn feed_prices(book: &SimPriceBook, market: &watch::Sender<MarketPrices>, observations: &[FundingObservation]) {
    let fresh: Vec<&FundingObservation> = observations.iter().filter(|o| o.data_status == DataStatus::Listed && o.mark_price > Decimal::ZERO).collect();
    if fresh.is_empty() {
        return;
    }
    for o in &fresh {
        book.set(o.exchange, &o.symbol, o.mark_price);
    }
    market.send_modify(|m| {
        for o in &fresh {
            m.insert((o.exchange, o.symbol.clone()), o.mark_price);
        }
    });
}

/// Account view used when no signed transport exists: every read fails (fail closed).
pub struct NoAccount(pub String);

impl AccountView for NoAccount {
    fn positions(&self, _: Exchange) -> BoxFut<'_, Result<Listed<AccountPosition>, String>> {
        Box::pin(std::future::ready(Err(self.0.clone())))
    }
    fn open_orders(&self, _: Exchange) -> BoxFut<'_, Result<Listed<AccountOrder>, String>> {
        Box::pin(std::future::ready(Err(self.0.clone())))
    }
    fn available_margin(&self, _: Exchange) -> BoxFut<'_, Result<Decimal, String>> {
        Box::pin(std::future::ready(Err(self.0.clone())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::sim::SimPrices;
    use crate::exchange::public::adapter::testkit::{block_on, fixture, ClockedTransport};
    use crate::exchange::public::adapter::InstrumentRules;
    use crate::exchange::public::binance::BinanceAdapter;
    use crate::exchange::public::bybit::BybitAdapter;
    use crate::exchange::public::okx::OkxAdapter;
    use crate::exchange::transport::{FakeTransport, HttpResponse};
    use crate::ports::{ManualClock, TimeSource};
    use crate::ui::testkit::{d, obs, with_status};
    use tong_funding_core::quantity::LotSize;

    const NOW: i64 = 1_791_201_300_000;

    fn rules(ex: Exchange, ct_val: Option<&str>) -> InstrumentRules {
        InstrumentRules {
            exchange: ex,
            symbol: "BTCUSDT".into(),
            step_size: d("0.1"),
            min_qty: d("0.2"),
            max_qty: None,
            market_step_size: Some(d("0.01")),
            market_min_qty: None,
            market_max_qty: None,
            min_notional: None,
            ct_val: ct_val.map(d),
            ct_mult: None,
        }
    }

    #[test]
    fn fresh_quote_uses_the_mark_price_and_the_receive_time() {
        let q = fresh_quote(obs(Exchange::Bybit, "BTCUSDT", "0.0001", 28_800, 9, 1_234));
        assert_eq!((q.price, q.price_observed_at_ms, q.listed), (d("100"), 1_234, true));
        let q = fresh_quote(with_status(obs(Exchange::Bybit, "BTCUSDT", "0.0001", 28_800, 9, 1_234), DataStatus::NotListed));
        assert!(!q.listed);
    }

    #[test]
    fn order_rules_use_the_market_lot_and_never_a_default() {
        let r = order_rules_of(Exchange::Binance, RulesLookup::Available(rules(Exchange::Binance, None))).unwrap();
        assert_eq!(r.lot, LotSize { step_size: d("0.01"), min_qty: d("0.2") }, "MARKET_LOT_SIZE, per-field fallback");
        assert_eq!(r.okx_ct_val, None);
        let okx = order_rules_of(Exchange::Okx, RulesLookup::Available(rules(Exchange::Okx, Some("0.01")))).unwrap();
        assert_eq!(okx.okx_ct_val, Some(d("0.01")));
        assert!(order_rules_of(Exchange::Okx, RulesLookup::Available(rules(Exchange::Okx, None))).is_err());
        assert_eq!(order_rules_of(Exchange::Bybit, RulesLookup::Unavailable { reason: "x".into() }), Err("x".into()));
        assert!(order_rules_of(Exchange::Bybit, RulesLookup::UnknownSymbol).is_err());
    }

    fn ok(rel: &str) -> Result<HttpResponse, crate::exchange::error::AdapterError> {
        Ok(HttpResponse::ok(fixture(rel)))
    }

    #[test]
    fn the_market_port_refetches_one_symbol_per_exchange_and_reads_rules() {
        let clock = ManualClock::new(NOW);
        let t = |f: FakeTransport| Arc::new(ClockedTransport::new(f, clock.clone(), 0));
        let binance = FakeTransport::new()
            .on("premiumIndex?symbol=BTCUSDT", ok("binance/premiumIndex_single_btcusdt.json"))
            .on("ticker/24hr?symbol=BTCUSDT", ok("binance/ticker24hr_single_btcusdt.json"))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"));
        let bybit = FakeTransport::new()
            .on("tickers?category=linear&symbol=BTCUSDT", ok("bybit/tickers_single_btcusdt.json"))
            .on("/v5/market/instruments-info", ok("bybit/instruments_complete.json"));
        let okx = FakeTransport::new()
            .on("funding-rate?instId=BTC-USDT-SWAP", ok("okx/funding_rate_btc.json"))
            .on("mark-price?instId=BTC-USDT-SWAP", ok("okx/mark_price_btc.json"))
            .on("market/ticker?instId=BTC-USDT-SWAP", ok("okx/ticker_btc.json"))
            .on("public/instruments?instType=SWAP", ok("okx/instruments_swap.json"));
        let c: Arc<dyn crate::ports::Clock> = Arc::new(clock.clone());
        let m = PublicMarketData { adapters: Arc::new((BinanceAdapter::new(t(binance), c.clone()), BybitAdapter::new(t(bybit), c.clone()), OkxAdapter::new(t(okx), c))) };
        for ex in Exchange::ALL {
            let q = block_on(m.refetch(ex, "BTCUSDT")).unwrap_or_else(|e| panic!("{ex:?}: {e}"));
            assert_eq!((q.funding.exchange, q.funding.symbol.as_str()), (ex, "BTCUSDT"));
            assert_eq!(q.price_observed_at_ms, NOW);
            assert!(q.listed);
        }
        let okx_rules = block_on(m.order_rules(Exchange::Okx, "BTCUSDT")).unwrap();
        assert!(okx_rules.okx_ct_val.is_some(), "OKX rules are in contracts");
        assert!(block_on(m.order_rules(Exchange::Binance, "BTCUSDT")).is_ok());
    }

    #[test]
    fn offsets_come_from_the_clock_syncs_and_unsynced_is_none() {
        let time: Arc<dyn TimeSource> = Arc::new(ManualClock::new(NOW));
        let offsets = ClockOffsets { clocks: Exchange::ALL.into_iter().map(|e| (e, Arc::new(ClockSync::new(time.clone())))).collect() };
        for e in Exchange::ALL {
            assert_eq!(offsets.offset_ms(e), None, "never synced: no entry (fail closed)");
        }
        assert_eq!(ClockOffsets { clocks: BTreeMap::new() }.offset_ms(Exchange::Bybit), None);
    }

    #[test]
    fn listed_prices_feed_the_simulator_and_the_engine_market_watch() {
        let book = SimPriceBook::default();
        let (tx, rx) = watch::channel(MarketPrices::new());
        let mut a = obs(Exchange::Binance, "BTCUSDT", "0.0001", 28_800, 9, 1);
        a.mark_price = d("60200");
        let gone = with_status(obs(Exchange::Bybit, "OLDUSDT", "0.0001", 28_800, 9, 1), DataStatus::NotListed);
        feed_prices(&book, &tx, &[a, gone]);
        assert_eq!(book.latest(Exchange::Binance, "BTCUSDT"), Some(d("60200")));
        assert_eq!(book.latest(Exchange::Bybit, "OLDUSDT"), None, "unlisted symbols are not priced");
        assert_eq!(rx.borrow().get(&(Exchange::Binance, "BTCUSDT".to_string())), Some(&d("60200")));
        assert_eq!(rx.borrow().len(), 1);
    }

    /// Headless subcommands (import / secrets / config) exit before the composition root opens the
    /// window, the store for the UI, or the engine.
    #[test]
    fn headless_subcommands_run_before_the_engine_starts() {
        let main = include_str!("../main.rs");
        let engine_at = main.find("LiveSource::start").expect("composition root present");
        for sub in ["import_cli::SUBCOMMAND", "secrets_cli::SUBCOMMAND", "config_cli::SUBCOMMAND"] {
            let at = main.find(sub).unwrap_or_else(|| panic!("{sub} dispatched"));
            assert!(at < engine_at, "{sub} must be dispatched before the engine starts");
        }
        assert_eq!(main.matches("LiveSource::start").count(), 1, "one composition root");
        assert_eq!(main.matches("Db::open_default").count(), 1, "one store instance for the UI and the engine");
    }

    #[test]
    fn no_account_fails_every_read() {
        let a = NoAccount("signed transport unavailable".into());
        assert!(block_on(a.available_margin(Exchange::Binance)).is_err());
        assert!(block_on(a.positions(Exchange::Bybit)).is_err());
    }
}
