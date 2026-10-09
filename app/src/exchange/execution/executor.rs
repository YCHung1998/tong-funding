//! `DemoExecutor`: the real `engine::ports::Executor` for EXCHANGE_DEMO (Binance + Bybit demo /
//! testnet, and OKX Demo Trading when the factory found OKX keys; otherwise OKX orders are not sent). Built only by
//! `factory::DemoExecutorFactory` when switching to EXCHANGE_DEMO (design D6).
//!
//! OKX opens that carry a leverage set it (cross, net mode) after the account-mode read that proves the
//! demo key and before the order; any failure there means the order is not sent.
//!
//! Before every order: the id must be a `demo` engine id, the exchange clock must be calibrated
//! (signing timestamp), the shared rate limiter must not be backing off, and the position mode
//! must be one-way (a confirmed reading is reused for [`POSITION_MODE_TTL_MS`]; an unknown or
//! hedge reading is never cached). Each of these failing means the order is NOT sent, which is a
//! certain rejection. Cancel accepts only ids recorded in `order_intents` ([`IntentLedger`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tong_funding_core::types::{Decimal, Exchange};

use super::binance::{BinanceOrderClient, ModeReading};
use super::bybit::BybitOrderClient;
use super::classify::{LeverageOutcome, OKX_NO_POSITION_CODES, SubmitClass};
use super::http::OrderTransport;
use super::okx::{OKX_LEG_CAP_FACTOR, OkxLimitsSource, OkxOrderClient, check_size};
use super::order::{ClientOrderId, OrderRef, ValidOrder};
use crate::engine::ports::{BoxFut, Executor, OrderRequest, QueryOutcome, ServerOffsets, SubmitOutcome};
use crate::exchange::health::ratelimit::{BackoffState, RateLimiter, RequestClass};
use crate::ports::TimeSource;
use crate::store::db::Db;

/// How long a confirmed one-way reading is trusted (defined in the signed layer, shared with the OKX read gate).
pub use crate::exchange::signed::okx::POSITION_MODE_TTL_MS;

/// Reason of OKX order calls on an executor built without an OKX client and without a reason.
pub const OKX_UNAVAILABLE: &str = "OKX order client not configured";

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
    /// OKX is optional: without keys the factory gives a reason instead and OKX orders are not sent.
    okx: Option<OkxOrderClient<T>>,
    okx_unavailable: String,
    /// Per-instrument size limits for OKX; without a source OKX opens are not sent (fail closed).
    okx_limits: Option<Arc<dyn OkxLimitsSource>>,
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
        DemoExecutor { binance, bybit, okx: None, okx_unavailable: OKX_UNAVAILABLE.to_string(), okx_limits: None, clock, offsets, intents, limiter, one_way_at: Mutex::new(HashMap::new()) }
    }

    /// Adds the OKX client. Its latch is the client's own: production attaches the factory's single
    /// latch to the client when it builds it (`DemoExecutorFactory::build`), so there is no
    /// builder-order-dependent second latch here.
    pub fn with_okx(mut self, okx: OkxOrderClient<T>) -> Self {
        self.okx = Some(okx);
        self
    }

    pub fn with_okx_limits(mut self, limits: Arc<dyn OkxLimitsSource>) -> Self {
        self.okx_limits = Some(limits);
        self
    }

    /// OKX orders answer `not_sent` with `reason` (never contains a key value).
    pub fn with_okx_unavailable(mut self, reason: String) -> Self {
        self.okx = None;
        self.okx_unavailable = reason;
        self
    }

    /// The OKX client, or the reason there is none.
    fn okx(&self) -> Result<&OkxOrderClient<T>, String> {
        self.okx.as_ref().ok_or_else(|| self.okx_unavailable.clone())
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
            Exchange::Binance | Exchange::Okx => (exchange, "*".to_string()), // account-wide setting
            Exchange::Bybit => (exchange, symbol.to_string()),
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
            Exchange::Okx => match self.okx() {
                Ok(okx) => okx.position_mode(timestamp).await,
                Err(reason) => Err(reason),
            },
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

    /// The four-way classification (the engine sees `into_outcome`). An OKX close (`reduce_only`)
    /// that is rejected for ANY reason, sent or not, says the opposite leg is naked.
    pub async fn submit_classified(&self, req: &OrderRequest) -> SubmitClass {
        match self.submit_inner(req).await {
            SubmitClass::Rejected { code, message } if req.exchange == Exchange::Okx && req.reduce_only => SubmitClass::Rejected {
                message: format!("OKX close refused ({code}: {message}): the opposite leg is naked (still open) and needs manual action"),
                code,
            },
            // "no position to reduce" is not by itself a naked leg: the wording says so (classification unchanged)
            SubmitClass::Unknown { reason } if req.exchange == Exchange::Okx && req.reduce_only && OKX_NO_POSITION_CODES.iter().any(|c| reason.contains(&format!("okx {c}:"))) => {
                SubmitClass::Unknown {
                    reason: format!("{reason}; likely no OKX position to close (already flat?); this alone says nothing about the opposite leg: check the OKX position and the opposite leg"),
                }
            }
            other => other,
        }
    }

    /// Everything the executor refuses without a request: the OKX client, the order's own validity and the
    /// OKX size guard. The leg cap comes from the pair's own notional when the request carries it (a later
    /// change of the template must not strand a staged pair), else from the live limits (manual orders).
    fn local_checks(&self, req: &OrderRequest) -> Result<ValidOrder, String> {
        if req.exchange == Exchange::Okx {
            self.okx()?;
        }
        let order = ValidOrder::from_request(req)?;
        if req.exchange == Exchange::Okx {
            // size guard first: nothing is requested for an order that is not a sane number of contracts
            // (a close is only checked for sz > 0 and the lot rule: never blocked by the cap or by missing data)
            let mut limits = self.okx_limits.as_ref().and_then(|l| l.limits(order.symbol()));
            if self.okx_limits.is_none() && !req.reduce_only {
                return Err("OKX size limits source not configured; OKX order not sent".into());
            }
            if let (Some(l), Some(n)) = (limits.as_mut(), req.notional_usdt) {
                l.max_leg_notional = n * Decimal::from(OKX_LEG_CAP_FACTOR);
            }
            check_size(order.quantity(), req.intended_base_qty, req.reduce_only, limits.as_ref())?;
        }
        Ok(order)
    }

    /// Clock, back-off and the one-way mode (for OKX this read is also the demo-key proof). Returns the
    /// signing timestamp.
    async fn gate(&self, req: &OrderRequest, order: &ValidOrder) -> Result<i64, String> {
        if req.exchange == Exchange::Okx && req.reduce_only {
            // a close must not trust a cached mode reading (a wrong mode leaves the other leg naked)
            self.one_way_at.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&(Exchange::Okx, "*".to_string()));
        }
        let Some(ts) = self.timestamp(req.exchange) else {
            return Err(format!("{} clock offset not calibrated", req.exchange.name()));
        };
        self.backoff(req.exchange)?;
        self.ensure_one_way(req.exchange, order.symbol(), ts).await?;
        Ok(ts)
    }

    /// order-leverage-sync: the leverage of an opening order, set on its exchange (nothing for an order
    /// without one). A failure means the order must not be sent.
    async fn apply_leverage(&self, req: &OrderRequest, order: &ValidOrder, ts: i64) -> Result<(), String> {
        let Some(leverage) = order.leverage() else { return Ok(()) };
        let ts = self.timestamp(req.exchange).unwrap_or(ts);
        let outcome = match req.exchange {
            Exchange::Binance => self.binance.set_leverage(order.symbol(), leverage, ts).await,
            Exchange::Bybit => self.bybit.set_leverage(order.symbol(), leverage, ts).await,
            // after the mode read (the demo-key proof) and the size guard: the same gates as the order
            Exchange::Okx => self.okx()?.set_leverage(order.symbol(), leverage, ts).await,
        };
        match outcome {
            LeverageOutcome::Set => {
                self.limiter.on_success(req.exchange, RequestClass::Signed);
                Ok(())
            }
            LeverageOutcome::RateLimited { retry_after_ms } => {
                self.note_rate_limit(req.exchange, Some(retry_after_ms));
                Err(format!("leverage not set (rate limited, retry after {retry_after_ms:?} ms)"))
            }
            LeverageOutcome::Failed { reason } => Err(format!("leverage {leverage}x not set on {}: {reason}", req.exchange.name())),
        }
    }

    /// The pre-send barrier for a pair's opening legs (okx-trading-enablement F1 / F2): every leg's local
    /// checks, then every leg's gate (clock, mode, demo-key proof), then the leverage on EVERY leg. Only
    /// when all of it passed may the engine send either order; the legs' leverage is remembered so the
    /// submits do not set it again. Any failure sends no order (setting a leverage opens no exposure).
    pub async fn prepare_legs(&self, legs: &[OrderRequest]) -> Result<(), String> {
        let mut checked = Vec::new();
        for req in legs {
            let order = self.local_checks(req).map_err(|e| format!("{} {}: {e}", req.exchange.name(), req.symbol))?;
            checked.push((req, order));
        }
        let mut stamped = Vec::new();
        for (req, order) in &checked {
            let ts = self.gate(req, order).await.map_err(|e| format!("{}: {e}", req.exchange.name()))?;
            stamped.push(ts);
        }
        for ((req, order), ts) in checked.iter().zip(stamped) {
            self.apply_leverage(req, order, ts).await?;
        }
        Ok(())
    }

    /// Sends `order` on its exchange and does the shared limiter bookkeeping. The OKX client's own hard stops
    /// (latch, demo-key proof) live inside `OkxOrderClient::submit`.
    async fn dispatch(&self, exchange: Exchange, order: &ValidOrder, ts: i64) -> SubmitClass {
        let class = match exchange {
            Exchange::Binance => self.binance.submit(order, ts).await,
            Exchange::Bybit => self.bybit.submit(order, ts).await,
            Exchange::Okx => match self.okx() {
                Ok(okx) => okx.submit(order, ts).await,
                Err(reason) => SubmitClass::Rejected { code: "not_sent".into(), message: reason },
            },
        };
        match &class {
            SubmitClass::RateLimited { retry_after_ms } => self.note_rate_limit(exchange, Some(*retry_after_ms)),
            SubmitClass::Accepted(_) | SubmitClass::Rejected { .. } => self.limiter.on_success(exchange, RequestClass::Signed),
            SubmitClass::Unknown { .. } => {}
        }
        class
    }

    /// A leg the barrier approved with its pair: only what cannot change its meaning is re-checked here
    /// (the order is valid, the OKX client exists, the signing time uses the offset the engine snapshotted for
    /// both legs before sending either, the hard stops inside `OkxOrderClient::submit`: latch and demo-key
    /// proof, and the transport's demo-flag rule). The soft, time-varying checks (mode re-read, back-off, mark
    /// freshness, limits, leverage) were approved together with the other leg; repeating them per leg could
    /// send one leg and refuse the other. ANY non-success here is worded as a possibly naked opposite leg.
    async fn submit_prepared(&self, req: &OrderRequest) -> SubmitClass {
        let naked = |code: String, message: String| SubmitClass::Rejected {
            message: format!("{message}; the pre-send barrier had passed, so the opposite leg may already be sent and be naked: check it"),
            code,
        };
        if req.exchange == Exchange::Okx
            && let Err(reason) = self.okx()
        {
            return naked("not_sent".into(), reason);
        }
        let order = match ValidOrder::from_request(req) {
            Ok(o) => o,
            Err(e) => return naked("not_sent".into(), e),
        };
        // the offset the engine snapshotted for this leg when it released the pair (not re-read per leg)
        let Some(prepared) = req.prepared else { return naked("not_sent".into(), "prepared leg without its barrier snapshot".into()) };
        let ts = self.clock.now_ms().saturating_add(prepared.offset_ms);
        match self.dispatch(req.exchange, &order, ts).await {
            SubmitClass::Rejected { code, message } => naked(code, message),
            other => other,
        }
    }

    async fn submit_inner(&self, req: &OrderRequest) -> SubmitClass {
        if req.prepared.is_some() && !req.reduce_only {
            return self.submit_prepared(req).await;
        }
        let not_sent = |message: String| SubmitClass::Rejected { code: "not_sent".into(), message };
        let order = match self.local_checks(req) {
            Ok(o) => o,
            Err(e) => return not_sent(e),
        };
        let ts = match self.gate(req, &order).await {
            Ok(ts) => ts,
            Err(e) => return not_sent(e),
        };
        if let Err(e) = self.apply_leverage(req, &order, ts).await {
            return not_sent(e);
        }
        let ts = self.timestamp(req.exchange).unwrap_or(ts);
        self.dispatch(req.exchange, &order, ts).await
    }

    /// Look an order up by `client_order_id` or by the exchange's order id.
    pub async fn query_by(&self, exchange: Exchange, symbol: &str, by: &OrderRef) -> QueryOutcome {
        if exchange == Exchange::Okx
            && let Err(reason) = self.okx()
        {
            return QueryOutcome::Failed { reason };
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
            Exchange::Okx => match self.okx() {
                Ok(okx) => okx.query(symbol, by, self.stamp(exchange)).await,
                Err(reason) => QueryOutcome::Failed { reason },
            },
        };
        self.note_query(exchange, &outcome);
        outcome
    }

    async fn cancel_own(&self, exchange: Exchange, symbol: &str, client_order_id: &str) -> QueryOutcome {
        if exchange == Exchange::Okx
            && let Err(reason) = self.okx()
        {
            return QueryOutcome::Failed { reason };
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
            Exchange::Okx => match self.okx() {
                Ok(okx) => okx.cancel(symbol, &id, self.stamp(exchange)).await,
                Err(reason) => QueryOutcome::Failed { reason },
            },
        };
        self.note_query(exchange, &outcome);
        outcome
    }
}

impl<T: OrderTransport + 'static> Executor for DemoExecutor<T> {
    fn is_simulated(&self) -> bool {
        false
    }

    fn prepare_open(&self, legs: Vec<OrderRequest>) -> BoxFut<'_, Result<(), String>> {
        Box::pin(async move { self.prepare_legs(&legs).await })
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
