//! The live-world executor factory (live-process 5.1/5.2): Binance and Bybit on the LIVE hosts
//! only, over a live order transport; no OKX client exists on this side. `ExecutionMode::ExchangeDemo`
//! is the internal name of "send real orders for this process identity" (design decision 2), so that
//! is the mode this factory builds; its ids are `live`-prefixed and the executor refuses any other.
//! Credentials come from the injected `SecretProvider` (the LIVE Keychain item) at that moment.
//! The error text names the exchange and the reason only, never a key value.

use std::sync::Arc;

use tong_funding_core::risk::ExecutionMode;
use tong_funding_core::types::Exchange;

use super::binance::BinanceOrderClient;
use super::bybit::BybitOrderClient;
use super::executor::{DemoExecutor, IntentLedger};
use super::http::LiveOrderTransport;
use crate::engine::ports::{Executor, ExecutorFactory, ServerOffsets};
use crate::exchange::health::ratelimit::RateLimiter;
use crate::exchange::signed::endpoints::{BinanceLive, BybitLive};
use crate::exchange::signed::signing::{Credentials, load_credentials};
use crate::ports::{SecretProvider, TimeSource};

pub struct LiveExecutorFactory<T> {
    transport: Arc<T>,
    secrets: Arc<dyn SecretProvider>,
    clock: Arc<dyn TimeSource>,
    offsets: Arc<dyn ServerOffsets>,
    intents: Arc<dyn IntentLedger>,
    limiter: Arc<RateLimiter>,
}

impl<T: LiveOrderTransport + 'static> LiveExecutorFactory<T> {
    pub fn new(
        transport: Arc<T>,
        secrets: Arc<dyn SecretProvider>,
        clock: Arc<dyn TimeSource>,
        offsets: Arc<dyn ServerOffsets>,
        intents: Arc<dyn IntentLedger>,
        limiter: Arc<RateLimiter>,
    ) -> Self {
        LiveExecutorFactory { transport, secrets, clock, offsets, intents, limiter }
    }

    fn credentials(&self, exchange: Exchange) -> Result<Arc<Credentials>, String> {
        load_credentials(self.secrets.as_ref(), exchange, false)
            .map(Arc::new)
            .map_err(|reason| format!("{} live keys unavailable ({reason:?}); live executor not built", exchange.name()))
    }

    /// The concrete executor (tests reach `submit_classified` through it).
    pub fn build(&self) -> Result<DemoExecutor<T, BinanceLive, BybitLive>, String> {
        let binance = self.credentials(Exchange::Binance)?;
        let bybit = self.credentials(Exchange::Bybit)?;
        Ok(DemoExecutor::new(
            BinanceOrderClient::new(self.transport.clone(), binance, BinanceLive),
            BybitOrderClient::new(self.transport.clone(), bybit, BybitLive),
            self.clock.clone(),
            self.offsets.clone(),
            self.intents.clone(),
            self.limiter.clone(),
        ))
    }
}

impl<T: LiveOrderTransport + 'static> ExecutorFactory for LiveExecutorFactory<T> {
    fn create(&self, mode: ExecutionMode) -> Result<Arc<dyn Executor>, String> {
        match mode {
            ExecutionMode::ExchangeDemo => Ok(Arc::new(self.build()?)),
            ExecutionMode::Simulation => Err("the live executor factory builds real-order executors only".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ports::{Leg, OrderAction, OrderSide, SubmitOutcome};
    use crate::exchange::execution::executor_tests::*;
    use crate::exchange::execution::http::fake::{FakeOrderTransport, Reply};
    use crate::exchange::execution::http::Method;
    use crate::engine::ids::{IdPrefix, client_order_id};
    use crate::exchange::signed::endpoints::BINANCE_LIVE_HOST;
    use crate::ports::{ManualClock, MemorySecrets, SecretName};

    fn secrets(on: &[Exchange]) -> Arc<dyn SecretProvider> {
        let mut m = MemorySecrets::default();
        for e in on {
            m = m.with(*e, SecretName::ApiKey, KEY).with(*e, SecretName::ApiSecret, SECRET);
        }
        Arc::new(m)
    }

    fn factory(t: &FakeOrderTransport, keys: &[Exchange]) -> LiveExecutorFactory<FakeOrderTransport> {
        let clock = ManualClock::new(NOW);
        LiveExecutorFactory::new(Arc::new(t.clone()), secrets(keys), Arc::new(clock.clone()), Arc::new(Offsets(Some(0))), Arc::new(Ledger::default()), Arc::new(RateLimiter::new(Arc::new(clock))))
    }

    #[test]
    fn it_builds_for_real_orders_only_and_needs_both_live_keys_and_never_names_a_value() {
        let t = FakeOrderTransport::new();
        assert!(factory(&t, &[Exchange::Binance, Exchange::Bybit]).create(ExecutionMode::ExchangeDemo).is_ok());
        assert!(factory(&t, &[Exchange::Binance, Exchange::Bybit]).create(ExecutionMode::Simulation).is_err());
        for missing in [&[Exchange::Binance][..], &[Exchange::Bybit][..], &[][..]] {
            let e = factory(&t, missing).create(ExecutionMode::ExchangeDemo).err().unwrap();
            assert!(e.contains("live keys unavailable") && !e.contains(KEY) && !e.contains(SECRET), "{e}");
        }
        // OKX keys alone never build anything, and are never read
        assert!(factory(&t, &[Exchange::Okx]).create(ExecutionMode::ExchangeDemo).is_err());
    }

    #[tokio::test]
    async fn the_built_executor_sends_only_to_the_live_host_with_live_ids() {
        let t = FakeOrderTransport::new();
        t.on(Method::Get, "/fapi/v1/positionSide/dual", Reply::ok(r#"{"dualSidePosition":false}"#));
        t.on(Method::Post, "/fapi/v1/order", Reply::ok(&binance_ack("x", "FILLED", "0.019")));
        let ex = factory(&t, &[Exchange::Binance, Exchange::Bybit]).create(ExecutionMode::ExchangeDemo).unwrap();
        let id = client_order_id(IdPrefix::Live, "pair-1", Leg::Long, OrderAction::Open, 0);
        let out = ex.submit(req(Exchange::Binance, &id, OrderSide::Buy, "0.019", false)).await;
        assert!(matches!(out, SubmitOutcome::Accepted(_)), "{out:?}");
        assert!(!t.requests().is_empty());
        assert!(t.requests().iter().all(|r| r.full_url().starts_with(&format!("https://{BINANCE_LIVE_HOST}/"))));
    }
}
