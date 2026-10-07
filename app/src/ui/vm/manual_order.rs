//! Manual order page view-model (ui-trading-pages task 4.1; spec manual-order-page). A debug tool:
//! single-leg market orders and cancels, sent ONLY as engine commands. The engine picks the
//! order path by `execution_mode` (SIMULATION → the simulator, results tagged simulated; merged
//! engine-simulation spec), so the page does not disable submit in SIMULATION. Non-reduce-only
//! orders follow the engine gate (kill switch / halt); reduce-only ones always pass.

use serde_json::Value;
use tong_funding_core::quantity::Quantity;
use tong_funding_core::risk::ExecutionMode;
use tong_funding_core::types::{Decimal, Exchange};

use super::bridge::{CommandSink, LegAccount, UiSnapshot, TRADABLE_EXCHANGES};
use super::engine_view::{blocker_text, refusing_blockers};
use super::format;
use crate::engine::command::{Command, ManualOrder};
use crate::engine::ports::{AccountPosition, Listed, OrderSide};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualForm {
    pub exchange: Exchange,
    pub symbol: String,
    pub side: OrderSide,
    pub quantity: String,
    pub reduce_only: bool,
    /// Leverage text; required for opening orders, ignored for reduce-only ones.
    pub leverage: String,
}

impl Default for ManualForm {
    fn default() -> Self {
        ManualForm { exchange: Exchange::Binance, symbol: "BTCUSDT".into(), side: OrderSide::Buy, quantity: String::new(), reduce_only: false, leverage: "5".into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelForm {
    pub exchange: Exchange,
    pub symbol: String,
    /// The `client_order_id` this system sent the order with.
    pub order_id: String,
}

impl Default for CancelForm {
    fn default() -> Self {
        CancelForm { exchange: Exchange::Binance, symbol: "BTCUSDT".into(), order_id: String::new() }
    }
}

/// One stored manual result, as reported.
#[derive(Debug, Clone, PartialEq)]
pub struct ManualResult {
    pub at: i64,
    pub text: String,
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ManualVm {
    /// One panel per exchange; `Err` = disabled with the reason.
    pub panels: Vec<(Exchange, Result<(), String>)>,
    pub mode: Option<ExecutionMode>,
    pub env_text: String,
    /// The floored order quantity in the exchange's unit (CONTRACTS on OKX, where the form takes
    /// coins), or why there is none.
    pub rounded: Result<Decimal, String>,
    /// OKX: `3 張（≈ 0.03 BTC，≈ 1,800.00 USDT）`, the conversion of the typed coin amount.
    pub qty_note: Option<String>,
    /// The quantity as the confirmation writes it (`0.001 BTC`, `3 張（≈ 0.03 BTC）`).
    pub qty_text: Option<String>,
    /// OKX opening order: the coin amount the user typed (the executor's size guard compares
    /// `contracts x ctVal` with it). `None` otherwise.
    pub intended_base_qty: Option<Decimal>,
    pub est_notional: Option<Decimal>,
    /// The validated leverage of an opening order; `None` for reduce-only (ignored) or while invalid.
    pub leverage: Option<Decimal>,
    /// The exchange cap line of an opening order (symbol-leverage-cap).
    pub cap_text: Option<String>,
    pub submit_disabled: Vec<String>,
    pub cancel_disabled: Vec<String>,
    pub results: Vec<ManualResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ManualConfirm {
    pub exchange: Exchange,
    pub symbol: String,
    pub side: OrderSide,
    /// The order quantity in the exchange's unit (contracts on OKX).
    pub quantity: Decimal,
    pub intended_base_qty: Option<Decimal>,
    pub qty_text: String,
    pub est_notional: Option<Decimal>,
    pub reduce_only: bool,
    pub leverage: Option<Decimal>,
    pub cap_text: Option<String>,
    pub mode: ExecutionMode,
    pub env_text: String,
    pub warning: &'static str,
}

pub fn env_text(mode: Option<ExecutionMode>) -> String {
    match mode {
        Some(ExecutionMode::Simulation) => "SIMULATION：由模擬器成交，不會送到交易所".into(),
        Some(ExecutionMode::ExchangeDemo) => "EXCHANGE_DEMO：將對 demo / testnet 帳戶真實下單".into(),
        None => "引擎未啟動：無法送出".into(),
    }
}

fn panel_state(snap: &UiSnapshot, ex: Exchange) -> Result<(), String> {
    if snap.settings.risk.allowed_exchanges.contains(&ex) { Ok(()) } else { Err("此交易所未在 allowed_exchanges 中".into()) }
}

/// The order quantity of the form in the exchange's unit, with how to show it.
struct Sized {
    /// Contracts on OKX, base coin elsewhere.
    order_qty: Decimal,
    /// The coin amount the order stands for.
    base_qty: Decimal,
    /// The quantity as written in the confirmation.
    text: String,
    /// OKX: the conversion line (`N 張（≈ x BTC，≈ y USDT）`).
    note: Option<String>,
    /// OKX: what the user typed.
    typed: Option<Decimal>,
}

fn sized(form: &ManualForm, snap: &UiSnapshot, price: Option<Decimal>) -> Result<Sized, String> {
    let symbol = form.symbol.trim().to_ascii_uppercase();
    if symbol.is_empty() {
        return Err("Symbol 為空".into());
    }
    let raw: Decimal = form.quantity.trim().parse().map_err(|_| "Quantity 不是有效的數字".to_string())?;
    if raw <= Decimal::ZERO {
        return Err("Quantity 必須大於 0".into());
    }
    let rules = match snap.rules.get(&(form.exchange, symbol.clone())) {
        None => return Err("合約規格載入中".into()),
        Some(Err(e)) => return Err(format!("無法取得合約規格（{e}）")),
        Some(Ok(r)) => r,
    };
    let coin = format::base_coin(&symbol);
    let below = |e: tong_funding_core::quantity::QuantityError| match e {
        tong_funding_core::quantity::QuantityError::BelowMinimum { .. } => "低於最小下單量".to_string(),
        other => other.to_string(),
    };
    if form.exchange == Exchange::Okx {
        // OKX sizes in contracts: the form takes COINS, converted with ctVal and floored to lotSz.
        let ct_val = rules.okx_ct_val.filter(|c| *c > Decimal::ZERO).ok_or_else(|| "無法取得合約規格（ctVal）".to_string())?;
        let contracts = Quantity::okx_contracts(raw, ct_val, &rules.lot).map_err(below)?;
        let n = contracts.value();
        let base = (n * ct_val).normalize();
        let note = match price {
            Some(p) => format!("{} 張（≈ {base} {coin}，≈ {} USDT）", n.normalize(), format::money(n * ct_val * p, 2)),
            None => format!("{} 張（≈ {base} {coin}）", n.normalize()),
        };
        return Ok(Sized { order_qty: n, base_qty: n * ct_val, text: format!("{} 張（≈ {base} {coin}）", n.normalize()), note: Some(note), typed: Some(raw) });
    }
    let q = Quantity::round_down(raw, &rules.lot).map_err(below)?;
    Ok(Sized { order_qty: q.value(), base_qty: q.value(), text: format!("{} {coin}", q.value().normalize()), note: None, typed: None })
}

/// A whole-number leverage from 1 to 125 (the range both exchanges take).
pub fn parse_leverage(text: &str) -> Result<Decimal, String> {
    let bad = || "Leverage 必須是 1–125 的整數".to_string();
    let l: Decimal = text.trim().parse().map_err(|_| bad())?;
    if !l.fract().is_zero() || l < Decimal::ONE || l > Decimal::from(125) {
        return Err(bad());
    }
    Ok(l)
}

/// The notional a cap is requested for: the estimate rounded UP to the next 1,000 USDT (at least
/// 1,000), so the reading does not change with every price tick and is never for a smaller notional.
pub fn cap_request_notional(estimate: Decimal) -> Decimal {
    let k = Decimal::from(1000);
    ((estimate / k).ceil() * k).max(k)
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn results(snap: &UiSnapshot) -> Vec<ManualResult> {
    let mut out = Vec::new();
    for e in &snap.trade_events {
        let p: Value = serde_json::from_str(&e.payload).unwrap_or(Value::Null);
        let tag = if p.get("simulated").and_then(Value::as_bool) == Some(true) { "[模擬] " } else { "" };
        let id = s(&p, "client_order_id");
        let (text, ok) = match e.event_type.as_str() {
            "MANUAL_ORDER_RESULT" => match s(&p, "outcome") {
                "accepted" => (format!("{tag}下單成功：order id {}（{id}）", p.get("exchange_order_id").and_then(Value::as_str).unwrap_or("交易所未回報")), true),
                "rejected" => (format!("{tag}下單失敗：{}（{id}）", s(&p, "reason")), false),
                _ => (format!("{tag}下單結果未知：{}（{id}）", s(&p, "reason")), false),
            },
            "MANUAL_CANCEL_RESULT" => match s(&p, "result") {
                "found" if s(&p, "state") == "Cancelled" => (format!("{tag}撤單成功（{id}）"), true),
                "found" => (format!("{tag}撤單：訂單已是 {}，未撤銷（{id}）", s(&p, "state")), false),
                "not found" => (format!("{tag}撤單：交易所找不到此訂單（{id}）"), false),
                _ => (format!("{tag}撤單失敗：{}（{id}）", s(&p, "reason")), false),
            },
            _ => continue,
        };
        out.push(ManualResult { at: e.ts_ms, text, ok });
        if out.len() >= 10 {
            break;
        }
    }
    out
}

/// Builds the page. Pure.
pub fn build(form: &ManualForm, cancel: &CancelForm, snap: &UiSnapshot) -> ManualVm {
    let mode = snap.engine.as_ref().map(|e| e.execution_mode);
    let panels: Vec<(Exchange, Result<(), String>)> = TRADABLE_EXCHANGES.iter().map(|ex| (*ex, panel_state(snap, *ex))).collect();
    let price = snap.market.get(&form.exchange).and_then(|f| f.observations.iter().find(|o| o.symbol == form.symbol.trim().to_ascii_uppercase())).map(|o| o.mark_price);
    let r = sized(form, snap, price);
    let est_notional = r.as_ref().ok().zip(price).map(|(z, p)| z.base_qty * p);

    let mut submit_disabled = Vec::new();
    match &snap.engine {
        None => submit_disabled.push("引擎未啟動".into()),
        Some(e) => {
            if !form.reduce_only {
                submit_disabled.extend(refusing_blockers(e).iter().map(blocker_text));
            }
        }
    }
    if let Err(why) = panel_state(snap, form.exchange) {
        submit_disabled.push(why);
    }
    if let Err(why) = &r {
        submit_disabled.push(why.clone());
    }

    // manual-order-leverage: an opening order needs a valid leverage that fits the exchange cap.
    let (mut leverage, mut cap_text) = (None, None);
    if !form.reduce_only {
        match parse_leverage(&form.leverage) {
            Err(why) => submit_disabled.push(why),
            Ok(l) => {
                leverage = Some(l);
                let now_ms = snap.engine.as_ref().map_or(0, |e| e.now_ms);
                let cap = super::leverage_cap::leg(snap, form.exchange, &form.symbol.trim().to_ascii_uppercase(), est_notional.unwrap_or(Decimal::ZERO), now_ms);
                cap_text = Some(super::leverage_cap::leg_text(form.exchange, &cap, l));
                if let Some(why) = super::leverage_cap::leg_blocked_reason(form.exchange, &cap, l, mode) {
                    submit_disabled.push(why);
                }
            }
        }
    }

    let mut cancel_disabled = Vec::new();
    if snap.engine.is_none() {
        cancel_disabled.push("引擎未啟動".into());
    }
    if cancel.order_id.trim().is_empty() {
        cancel_disabled.push("Order ID 為空".into());
    }
    if cancel.symbol.trim().is_empty() {
        cancel_disabled.push("Symbol 為空".into());
    }

    let (qty_note, intended_base_qty, qty_text) = match &r {
        Ok(z) => (z.note.clone(), if form.reduce_only { None } else { z.typed }, Some(z.text.clone())),
        Err(_) => (None, None, None),
    };
    ManualVm { panels, mode, env_text: env_text(mode), rounded: r.map(|z| z.order_qty), qty_note, qty_text, intended_base_qty, est_notional, leverage, cap_text, submit_disabled, cancel_disabled, results: results(snap) }
}

/// Opens the confirmation; `None` while submit is disabled. Sends nothing.
pub fn open_confirm(vm: &ManualVm, form: &ManualForm) -> Option<ManualConfirm> {
    if !vm.submit_disabled.is_empty() {
        return None;
    }
    let mode = vm.mode?;
    let quantity = *vm.rounded.as_ref().ok()?;
    let symbol = form.symbol.trim().to_ascii_uppercase();
    let qty_text = vm.qty_text.clone()?;
    Some(ManualConfirm {
        exchange: form.exchange,
        symbol,
        side: form.side,
        quantity,
        intended_base_qty: vm.intended_base_qty,
        qty_text,
        est_notional: vm.est_notional,
        reduce_only: form.reduce_only,
        leverage: vm.leverage,
        cap_text: vm.cap_text.clone(),
        mode,
        env_text: env_text(Some(mode)),
        warning: "單腿下單不會自動建立對腿",
    })
}

/// After the confirmation: exactly one `ManualOrder` command (the engine's single order path).
pub fn confirm(c: &ManualConfirm, sink: &dyn CommandSink) {
    sink.send(
        format!("手動下單 {} {} {:?} {}", c.exchange.name(), c.symbol, c.side, c.qty_text),
        Command::ManualOrder(ManualOrder { exchange: c.exchange, symbol: c.symbol.clone(), side: c.side, quantity: c.quantity, reduce_only: c.reduce_only, leverage: if c.reduce_only { None } else { c.leverage }, intended_base_qty: if c.reduce_only { None } else { c.intended_base_qty } }),
    );
}

/// Cancel by order id through the engine; `false` (nothing sent) while disabled.
pub fn cancel(vm: &ManualVm, form: &CancelForm, sink: &dyn CommandSink) -> bool {
    if !vm.cancel_disabled.is_empty() {
        return false;
    }
    sink.send(
        format!("撤單 {} {}", form.exchange.name(), form.order_id.trim()),
        Command::ManualCancel { exchange: form.exchange, symbol: form.symbol.trim().to_ascii_uppercase(), client_order_id: form.order_id.trim().to_string() },
    );
    true
}

// ---- manual-order-position-picker -------------------------------------------------------------

/// What one exchange's list can honestly say: never a stale or guessed row.
#[derive(Debug, Clone, PartialEq)]
pub enum PickState<T> {
    /// No read of this account has arrived yet (not "no positions").
    NotQueried,
    /// The read failed; the message is shown and no rows are.
    Failed(String),
    /// `incomplete` = the exchange reported a partial list.
    Rows { rows: Vec<T>, incomplete: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PickList<T> {
    pub exchange: Exchange,
    pub fetched_at: Option<i64>,
    pub state: PickState<T>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PositionRow {
    pub exchange: Exchange,
    pub symbol: String,
    /// Signed COIN amount: long positive, short negative. OKX reports contracts; they are
    /// multiplied by `ctVal` here (and `ZERO` while `unconvertible`: never contracts as coins).
    pub quantity: Decimal,
    pub is_long: bool,
    /// OKX only: the signed contract count, when `ctVal` is known.
    pub contracts: Option<Decimal>,
    /// OKX position whose `ctVal` is unknown: it cannot be shown in coins or closed from here.
    pub unconvertible: bool,
    /// OKX only: the position as the exchange reported it (contracts), for the "無法換算" label.
    pub raw_contracts: Option<Decimal>,
}

impl PositionRow {
    /// Close = opposite side, the whole position in coins, reduce-only (the form converts OKX coins
    /// back to contracts). `None` while the position cannot be converted.
    pub fn close_prefill(&self) -> Option<ManualPrefill> {
        (!self.unconvertible).then(|| ManualPrefill {
            exchange: self.exchange,
            symbol: self.symbol.clone(),
            side: if self.is_long { OrderSide::Sell } else { OrderSide::Buy },
            quantity: self.quantity.abs().normalize().to_string(),
            reduce_only: true,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderRow {
    pub exchange: Exchange,
    pub symbol: String,
    pub order_id: Option<String>,
    /// Remaining COIN amount (OKX: contracts x `ctVal`; `ZERO` while `unconvertible`).
    pub remaining: Decimal,
    /// OKX only: the remaining contracts as reported.
    pub remaining_contracts: Option<Decimal>,
    pub unconvertible: bool,
}

/// Values a "close this position" click writes into the order form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualPrefill {
    pub exchange: Exchange,
    pub symbol: String,
    pub side: OrderSide,
    pub quantity: String,
    pub reduce_only: bool,
}

/// Values a "cancel this order" click writes into the cancel form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelPrefill {
    pub exchange: Exchange,
    pub symbol: String,
    pub order_id: String,
}

/// The account the engine trades against: simulated ledger unless EXCHANGE_DEMO (engine stopped
/// reads as simulated).
fn account_is_simulated(snap: &UiSnapshot) -> bool {
    snap.engine.as_ref().map(|e| e.execution_mode) != Some(ExecutionMode::ExchangeDemo)
}

fn pick_lists<A, T>(
    snap: &UiSnapshot,
    read: impl Fn(&LegAccount) -> &Result<Listed<A>, String>,
    row: impl Fn(&A) -> Option<T>,
) -> Vec<PickList<T>> {
    let simulated = account_is_simulated(snap);
    TRADABLE_EXCHANGES
        .iter()
        .map(|ex| match snap.leg_accounts.get(&(simulated, *ex)) {
            None => PickList { exchange: *ex, fetched_at: None, state: PickState::NotQueried },
            Some(acc) => PickList {
                exchange: *ex,
                fetched_at: Some(acc.fetched_at),
                state: match read(acc) {
                    Err(e) => PickState::Failed(e.clone()),
                    Ok(l) => PickState::Rows { rows: l.items.iter().filter_map(&row).collect(), incomplete: !l.complete },
                },
            },
        })
        .collect()
}

/// Non-zero positions per tradable exchange, from the account of the current execution mode.
pub fn open_positions(snap: &UiSnapshot) -> Vec<PickList<PositionRow>> {
    pick_lists(snap, |a| &a.positions, |p| {
        if p.quantity.is_zero() {
            return None;
        }
        let is_long = p.quantity > Decimal::ZERO;
        Some(match ct_val_of(snap, p.exchange, &p.symbol) {
            CtVal::NotOkx => PositionRow { exchange: p.exchange, symbol: p.symbol.clone(), quantity: p.quantity, is_long, contracts: None, unconvertible: false, raw_contracts: None },
            CtVal::Known(c) => PositionRow { exchange: p.exchange, symbol: p.symbol.clone(), quantity: p.quantity * c, is_long, contracts: Some(p.quantity), unconvertible: false, raw_contracts: Some(p.quantity) },
            CtVal::Unknown => PositionRow { exchange: p.exchange, symbol: p.symbol.clone(), quantity: Decimal::ZERO, is_long, contracts: None, unconvertible: true, raw_contracts: Some(p.quantity) },
        })
    })
}

/// How the quantities of one (exchange, symbol) convert to coins.
enum CtVal {
    NotOkx,
    Known(Decimal),
    Unknown,
}

fn ct_val_of(snap: &UiSnapshot, exchange: Exchange, symbol: &str) -> CtVal {
    if exchange != Exchange::Okx {
        return CtVal::NotOkx;
    }
    match snap.rules.get(&(exchange, symbol.to_string())) {
        Some(Ok(r)) => r.okx_ct_val.filter(|c| *c > Decimal::ZERO).map_or(CtVal::Unknown, CtVal::Known),
        _ => CtVal::Unknown,
    }
}

/// Open orders per tradable exchange; rows without an id are kept (shown, not pickable).
pub fn open_orders(snap: &UiSnapshot) -> Vec<PickList<OrderRow>> {
    pick_lists(snap, |a| &a.open_orders, |o| {
        let order_id = o.client_order_id.as_ref().map(|i| i.trim().to_string()).filter(|i| !i.is_empty());
        Some(match ct_val_of(snap, o.exchange, &o.symbol) {
            CtVal::NotOkx => OrderRow { exchange: o.exchange, symbol: o.symbol.clone(), order_id, remaining: o.remaining_quantity, remaining_contracts: None, unconvertible: false },
            CtVal::Known(c) => OrderRow { exchange: o.exchange, symbol: o.symbol.clone(), order_id, remaining: o.remaining_quantity * c, remaining_contracts: Some(o.remaining_quantity), unconvertible: false },
            CtVal::Unknown => OrderRow { exchange: o.exchange, symbol: o.symbol.clone(), order_id, remaining: Decimal::ZERO, remaining_contracts: Some(o.remaining_quantity), unconvertible: true },
        })
    })
}

/// Close prefill of an engine position that is already in COINS (Binance / Bybit). OKX positions
/// are contracts and are converted by `open_positions` first: this refuses them (`None`) so contracts
/// can never be written into the coin form.
pub fn close_prefill(p: &AccountPosition) -> Option<ManualPrefill> {
    (p.exchange != Exchange::Okx).then(|| ManualPrefill {
        exchange: p.exchange,
        symbol: p.symbol.clone(),
        side: if p.quantity > Decimal::ZERO { OrderSide::Sell } else { OrderSide::Buy },
        quantity: p.quantity.abs().normalize().to_string(),
        reduce_only: true,
    })
}

/// `None` for an order without an id (it cannot be cancelled from here).
pub fn cancel_prefill(o: &OrderRow) -> Option<CancelPrefill> {
    o.order_id.clone().map(|order_id| CancelPrefill { exchange: o.exchange, symbol: o.symbol.clone(), order_id })
}

#[cfg(test)]
#[path = "manual_order_tests.rs"]
mod tests;
