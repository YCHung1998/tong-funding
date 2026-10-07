//! `DemoExecutor`: the real `engine::ports::Executor` for EXCHANGE_DEMO (Binance + Bybit demo /
//! testnet; OKX answers "unsupported" without any request). Built only by
//! `factory::DemoExecutorFactory` when switching to EXCHANGE_DEMO (design D6).
//!
//! Before every order: the id must be a `demo` engine id, the exchange clock must be calibrated
//! (signing timestamp), the shared rate limiter must not be backing off, and the position mode
//! must be one-way (a confirmed reading is reused for [`POSITION_MODE_TTL_MS`]; an unknown or
//! hedge reading is never cached). Each of these failing means the order is NOT sent, which is a
//! certain rejection. Cancel accepts only ids recorded in `order_intents` ([`IntentLedger`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tong_funding_core::types::Exchange;

use super::binance::{BinanceOrderClient, ModeReading};
use super::bybit::BybitOrderClient;
use super::classify::{LeverageOutcome, SubmitClass};
use super::http::OrderTransport;
use super::order::{ClientOrderId, OrderRef, ValidOrder};
use crate::engine::ports::{BoxFut, Executor, OrderRequest, QueryOutcome, ServerOffsets, SubmitOutcome};
use crate::exchange::health::ratelimit::{BackoffState, RateLimiter, RequestClass};
use crate::ports::TimeSource;
use crate::store::db::Db;

/// How long a confirmed one-way reading is trusted (my choice, UNVERIFIED that the exchanges
/// refuse a mode switch with open positions; a switch inside the window is caught by the exchange
/// rejecting `positionIdx` 0 / reduce-only orders).
pub const POSITION_MODE_TTL_MS: i64 = 60_000;

/// Reason text of OKX order calls (no request is ever built for OKX).
pub const OKX_UNSUPPORTED: &str = "OKX order execution is unsupported (public market data only)";

/// Which `client_order_id`s belong to this system (`order_intents`).
pub trait IntentLedger: Send + Sync {
    fn owns(&self, client_order_id: &str) -> Result<bool, String>;
}

impl IntentLedger for Db {
    fn owns(&self, client_order_id: &str) -> Result<bool, String> {
        self.get_intent(client_order_id).map(|row| row.is_some()).map_err(|e| e.to_string())
    }
}

pub struct DemoExecutor<T> {
    binance: BinanceOrderClient<T>,
    bybit: BybitOrderClient<T>,
    clock: Arc<dyn TimeSource>,
    offsets: Arc<dyn ServerOffsets>,
    intents: Arc<dyn IntentLedger>,
    limiter: Arc<RateLimiter>,
    /// (exchange, symbol or "*") → local ms of the last confirmed one-way reading.
    one_way_at: Mutex<HashMap<(Exchange, String), i64>>,
}

impl<T: OrderTransport> DemoExecutor<T> {
    pub fn new(
        binance: BinanceOrderClient<T>,
        bybit: BybitOrderClient<T>,
        clock: Arc<dyn TimeSource>,
        offsets: Arc<dyn ServerOffsets>,
        intents: Arc<dyn IntentLedger>,
        limiter: Arc<RateLimiter>,
    ) -> Self {
        DemoExecutor { binance, bybit, clock, offsets, intents, limiter, one_way_at: Mutex::new(HashMap::new()) }
    }

    /// Exchange time for signing; `None` = never calibrated (nothing may be signed).
    fn timestamp(&self, exchange: Exchange) -> Option<i64> {
        self.offsets.offset_ms(exchange).map(|o| self.clock.now_ms().saturating_add(o))
    }

    fn stamp(&self, exchange: Exchange) -> impl Fn() -> i64 + '_ {
        move || self.timestamp(exchange).unwrap_or_else(|| self.clock.now_ms())
    }

    /// `Err(reason)` while the shared limiter is backing off this exchange's signed requests.
    fn backoff(&self, exchange: Exchange) -> Result<(), String> {
        match self.limiter.state(exchange, RequestClass::Signed) {
            BackoffState::Clear => Ok(()),
            BackoffState::Waiting { remaining_ms, .. } => Err(format!("rate limited: backing off for {remaining_ms} ms")),
        }
    }

    fn note_rate_limit(&self, exchange: Exchange, class_retry: Option<Option<u64>>) {
        if let Some(retry_after_ms) = class_retry {
            self.limiter.on_rate_limited(exchange, RequestClass::Signed, retry_after_ms);
        }
    }

    fn note_query(&self, exchange: Exchange, outcome: &QueryOutcome) {
        match outcome {
            QueryOutcome::Failed { reason } if reason.starts_with("rate limited") => {
                let retry = reason.split("Some(").nth(1).and_then(|s| s.split(')').next()).and_then(|s| s.parse::<u64>().ok());
                self.limiter.on_rate_limited(exchange, RequestClass::Signed, retry);
            }
            QueryOutcome::Found(_) | QueryOutcome::NotFound => self.limiter.on_success(exchange, RequestClass::Signed),
            QueryOutcome::Failed { .. } => {}
        }
    }

    /// One-way mode confirmed within the TTL, or read now. Any failure = do not send.
    async fn ensure_one_way(&self, exchange: Exchange, symbol: &str, timestamp: i64) -> Result<(), String> {
        let key = match exchange {
            Exchange::Binance => (exchange, "*".to_string()), // account-wide setting
            Exchange::Bybit | Exchange::Okx => (exchange, symbol.to_string()),
        };
        let now = self.clock.now_ms();
        if let Some(at) = self.one_way_at.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&key)
            && now - at < POSITION_MODE_TTL_MS
        {
            return Ok(());
        }
        let reading = match exchange {
            Exchange::Binance => self.binance.position_mode(timestamp).await,
            Exchange::Bybit => self.bybit.position_mode(symbol, timestamp).await,
            Exchange::Okx => Err(OKX_UNSUPPORTED.to_string()),
        };
        match reading {
            Ok(ModeReading::OneWay) => {
                self.one_way_at.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key, now);
                Ok(())
            }
            Ok(ModeReading::Hedge) => Err("position mode mismatch: account is in hedge mode, the system assumes one-way".into()),
            Err(e) => Err(format!("position mode not confirmed ({e})")),
        }
    }

    /// The four-way classification (the engine sees `into_outcome`).
    pub async fn submit_classified(&self, req: &OrderRequest) -> SubmitClass {
        let not_sent = |message: String| SubmitClass::Rejected { code: "not_sent".into(), message };
        if req.exchange == Exchange::Okx {
            return not_sent(OKX_UNSUPPORTED.into());
        }
        let order = match ValidOrder::from_request(req) {
            Ok(o) => o,
            Err(e) => return not_sent(e),
        };
        let Some(ts) = self.timestamp(req.exchange) else {
            return not_sent(format!("{} clock offset not calibrated", req.exchange.name()));
        };
        if let Err(e) = self.backoff(req.exchange) {
            return not_sent(e);
        }
        if let Err(e) = self.ensure_one_way(req.exchange, order.symbol(), ts).await {
            return not_sent(e);
        }
        // order-leverage-sync: an opening order leaves only after both exchanges hold the same
        // leverage; a failure here means nothing was sent (a certain rejection).
        if let Some(leverage) = order.leverage() {
            let ts = self.timestamp(req.exchange).unwrap_or(ts);
            let outcome = match req.exchange {
                Exchange::Binance => self.binance.set_leverage(order.symbol(), leverage, ts).await,
                Exchange::Bybit => self.bybit.set_leverage(order.symbol(), leverage, ts).await,
                Exchange::Okx => return not_sent(OKX_UNSUPPORTED.into()),
            };
            match outcome {
                LeverageOutcome::Set => self.limiter.on_success(req.exchange, RequestClass::Signed),
                LeverageOutcome::RateLimited { retry_after_ms } => {
                    self.note_rate_limit(req.exchange, Some(retry_after_ms));
                    return not_sent(format!("leverage not set (rate limited, retry after {retry_after_ms:?} ms)"));
                }
                LeverageOutcome::Failed { reason } => return not_sent(format!("leverage {leverage}x not set on {}: {reason}", req.exchange.name())),
            }
        }
        let ts = self.timestamp(req.exchange).unwrap_or(ts);
        let class = match req.exchange {
            Exchange::Binance => self.binance.submit(&order, ts).await,
            Exchange::Bybit => self.bybit.submit(&order, ts).await,
            Exchange::Okx => not_sent(OKX_UNSUPPORTED.into()),
        };
        match &class {
            SubmitClass::RateLimited { retry_after_ms } => self.note_rate_limit(req.exchange, Some(*retry_after_ms)),
            SubmitClass::Accepted(_) | SubmitClass::Rejected { .. } => self.limiter.on_success(req.exchange, RequestClass::Signed),
            SubmitClass::Unknown { .. } => {}
        }
        class
    }

    /// Look an order up by `client_order_id` or by the exchange's order id.
    pub async fn query_by(&self, exchange: Exchange, symbol: &str, by: &OrderRef) -> QueryOutcome {
        if exchange == Exchange::Okx {
            return QueryOutcome::Failed { reason: OKX_UNSUPPORTED.into() };
        }
        if self.timestamp(exchange).is_none() {
            return QueryOutcome::Failed { reason: format!("{} clock offset not calibrated", exchange.name()) };
        }
        if let Err(reason) = self.backoff(exchange) {
            return QueryOutcome::Failed { reason };
        }
        let outcome = match exchange {
            Exchange::Binance => self.binance.query(symbol, by, self.stamp(exchange)).await,
            Exchange::Bybit => self.bybit.query(symbol, by, self.stamp(exchange)).await,
            Exchange::Okx => QueryOutcome::Failed { reason: OKX_UNSUPPORTED.into() },
        };
        self.note_query(exchange, &outcome);
        outcome
    }

    async fn cancel_own(&self, exchange: Exchange, symbol: &str, client_order_id: &str) -> QueryOutcome {
        if exchange == Exchange::Okx {
            return QueryOutcome::Failed { reason: OKX_UNSUPPORTED.into() };
        }
        let id = match ClientOrderId::parse(client_order_id) {
            Ok(id) => id,
            Err(e) => return QueryOutcome::Failed { reason: format!("cancel refused: {e}") },
        };
        match self.intents.owns(id.as_str()) {
            Ok(true) => {}
            Ok(false) => return QueryOutcome::Failed { reason: "cancel refused: not an order of this system (not in order_intents)".into() },
            Err(e) => return QueryOutcome::Failed { reason: format!("cancel refused: order intents unreadable ({e})") },
        }
        let Some(ts) = self.timestamp(exchange) else {
            return QueryOutcome::Failed { reason: format!("{} clock offset not calibrated", exchange.name()) };
        };
        if let Err(reason) = self.backoff(exchange) {
            return QueryOutcome::Failed { reason };
        }
        let outcome = match exchange {
            Exchange::Binance => self.binance.cancel(symbol, &id, ts).await,
            Exchange::Bybit => self.bybit.cancel(symbol, &id, self.stamp(exchange)).await,
            Exchange::Okx => QueryOutcome::Failed { reason: OKX_UNSUPPORTED.into() },
        };
        self.note_query(exchange, &outcome);
        outcome
    }
}

impl<T: OrderTransport + 'static> Executor for DemoExecutor<T> {
    fn is_simulated(&self) -> bool {
        false
    }

    fn submit(&self, req: OrderRequest) -> BoxFut<'_, SubmitOutcome> {
        Box::pin(async move { self.submit_classified(&req).await.into_outcome() })
    }

    fn cancel(&self, exchange: Exchange, symbol: &str, client_order_id: &str) -> BoxFut<'_, QueryOutcome> {
        let (symbol, id) = (symbol.to_string(), client_order_id.to_string());
        Box::pin(async move { self.cancel_own(exchange, &symbol, &id).await })
    }

    fn query(&self, exchange: Exchange, symbol: &str, client_order_id: &str) -> BoxFut<'_, QueryOutcome> {
        let (symbol, id) = (symbol.to_string(), client_order_id.to_string());
        Box::pin(async move {
            match ClientOrderId::parse(&id) {
                Ok(cid) => self.query_by(exchange, &symbol, &OrderRef::Client(cid)).await,
                Err(e) => QueryOutcome::Failed { reason: format!("query refused: {e}") },
            }
        })
    }
}
