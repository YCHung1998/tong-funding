//! `DemoExecutorFactory`: the real `engine::ports::ExecutorFactory` (design D6). It builds the
//! order-capable [`DemoExecutor`] only for `EXCHANGE_DEMO`, with credentials read from the
//! injected `SecretProvider` (the macOS Keychain in production) at that moment. Missing, empty
//! or unreadable keys for Binance OR Bybit -> `Err` (the engine stays in SIMULATION); an empty key
//! is never used to sign. Both are required because OKX cannot trade, so every demo pair needs
//! both. The error text names the exchange and the reason only, never a key value.

use std::sync::Arc;

use tong_funding_core::risk::ExecutionMode;
use tong_funding_core::types::Exchange;

use super::binance::BinanceOrderClient;
use super::bybit::BybitOrderClient;
use super::executor::{DemoExecutor, IntentLedger};
use super::http::OrderTransport;
use crate::engine::ports::{Executor, ExecutorFactory, ServerOffsets};
use crate::exchange::health::ratelimit::RateLimiter;
use crate::exchange::signed::endpoints::{BinanceHost, BybitHost};
use crate::exchange::signed::signing::{Credentials, load_credentials};
use crate::ports::{SecretProvider, TimeSource};

pub struct DemoExecutorFactory<T> {
    transport: Arc<T>,
    secrets: Arc<dyn SecretProvider>,
    clock: Arc<dyn TimeSource>,
    offsets: Arc<dyn ServerOffsets>,
    intents: Arc<dyn IntentLedger>,
    limiter: Arc<RateLimiter>,
    binance_env: BinanceHost,
}

impl<T: OrderTransport + 'static> DemoExecutorFactory<T> {
    /// `binance_env` picks one of the two compile-time Binance demo hosts (design Open Question 1:
    /// which one accepts the user's key is decided in task 4.2). Bybit has a single demo host.
    pub fn new(
        transport: Arc<T>,
        secrets: Arc<dyn SecretProvider>,
        clock: Arc<dyn TimeSource>,
        offsets: Arc<dyn ServerOffsets>,
        intents: Arc<dyn IntentLedger>,
        limiter: Arc<RateLimiter>,
        binance_env: BinanceHost,
    ) -> Self {
        DemoExecutorFactory { transport, secrets, clock, offsets, intents, limiter, binance_env }
    }

    fn credentials(&self, exchange: Exchange) -> Result<Arc<Credentials>, String> {
        load_credentials(self.secrets.as_ref(), exchange, false)
            .map(Arc::new)
            .map_err(|reason| format!("{} keys unavailable ({reason:?}); demo executor not built", exchange.name()))
    }

    /// The concrete executor (tests use it to reach `submit_classified` / `query_by`).
    pub fn build(&self) -> Result<DemoExecutor<T>, String> {
        let binance = self.credentials(Exchange::Binance)?;
        let bybit = self.credentials(Exchange::Bybit)?;
        Ok(DemoExecutor::new(
            BinanceOrderClient::new(self.transport.clone(), binance, self.binance_env),
            BybitOrderClient::new(self.transport.clone(), bybit, BybitHost::Demo),
            self.clock.clone(),
            self.offsets.clone(),
            self.intents.clone(),
            self.limiter.clone(),
        ))
    }
}

impl<T: OrderTransport + 'static> ExecutorFactory for DemoExecutorFactory<T> {
    fn create(&self, mode: ExecutionMode) -> Result<Arc<dyn Executor>, String> {
        match mode {
            ExecutionMode::ExchangeDemo => Ok(Arc::new(self.build()?)),
            ExecutionMode::Simulation => Err("the demo executor factory builds EXCHANGE_DEMO executors only".into()),
        }
    }
}
