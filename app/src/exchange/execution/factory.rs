//! `DemoExecutorFactory`: the real `engine::ports::ExecutorFactory` (design D6). It builds the
//! order-capable [`DemoExecutor`] only for `EXCHANGE_DEMO`, with credentials read from the
//! injected `SecretProvider` (the macOS Keychain in production) at that moment. Missing, empty
//! or unreadable keys for Binance OR Bybit -> `Err` (the engine stays in SIMULATION); an empty key
//! is never used to sign. Binance and Bybit are required; OKX (key, secret and passphrase) is
//! optional: without it the executor is still built and OKX orders are not sent, with the reason.
//! The error text names the exchange and the reason only, never a key value.

use std::sync::Arc;

use tong_funding_core::risk::ExecutionMode;
use tong_funding_core::types::Exchange;

use super::binance::BinanceOrderClient;
use super::bybit::BybitOrderClient;
use super::executor::{DemoExecutor, IntentLedger};
use super::http::OrderTransport;
use super::okx::{OkxLimitsSource, OkxOrderClient};
use crate::engine::ports::{Executor, ExecutorFactory, ServerOffsets};
use crate::exchange::health::ratelimit::RateLimiter;
use crate::exchange::signed::endpoints::{BinanceHost, BybitHost, OkxHost};
use crate::exchange::signed::okx::OkxLatch;
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
    okx_limits: Option<Arc<dyn OkxLimitsSource>>,
    /// The one OKX latch of the process: every executor this factory builds shares it, and the
    /// read client gets the same `Arc` from `okx_latch()` (okx-trading-enablement 3.5).
    okx_latch: Arc<OkxLatch>,
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
        DemoExecutorFactory { transport, secrets, clock, offsets, intents, limiter, binance_env, okx_limits: None, okx_latch: OkxLatch::new() }
    }

    /// The process-wide OKX latch (a `50101` anywhere disables OKX everywhere).
    pub fn okx_latch(&self) -> Arc<OkxLatch> {
        self.okx_latch.clone()
    }

    /// Where the OKX size guard gets `ctVal` / `lotSz` / mark price / the notional cap. Without it
    /// OKX orders are not sent (fail closed); the production wiring is okx-trading-enablement.
    pub fn with_okx_limits(mut self, limits: Arc<dyn OkxLimitsSource>) -> Self {
        self.okx_limits = Some(limits);
        self
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
        let executor = DemoExecutor::new(
            BinanceOrderClient::new(self.transport.clone(), binance, self.binance_env),
            BybitOrderClient::new(self.transport.clone(), bybit, BybitHost::Demo),
            self.clock.clone(),
            self.offsets.clone(),
            self.intents.clone(),
            self.limiter.clone(),
        );
        // OKX is optional (design D5): key, secret and passphrase, else OKX orders are not sent.
        let executor = match &self.okx_limits {
            Some(l) => executor.with_okx_limits(l.clone()),
            None => executor,
        };
        Ok(match load_credentials(self.secrets.as_ref(), Exchange::Okx, true) {
            Ok(okx) => executor.with_okx(OkxOrderClient::new(self.transport.clone(), Arc::new(okx), OkxHost::Demo).with_latch(self.okx_latch.clone())),
            Err(reason) => executor.with_okx_unavailable(format!("OKX keys unavailable ({reason:?}); OKX order not sent")),
        })
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
