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
}

impl Default for ManualForm {
    fn default() -> Self {
        ManualForm { exchange: Exchange::Binance, symbol: "BTCUSDT".into(), side: OrderSide::Buy, quantity: String::new(), reduce_only: false }
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
    /// Binance and Bybit only (OKX takes no orders); `Err` = disabled with the reason.
    pub panels: Vec<(Exchange, Result<(), String>)>,
    pub mode: Option<ExecutionMode>,
    pub env_text: String,
    /// The floored quantity, or why there is none.
    pub rounded: Result<Decimal, String>,
    pub est_notional: Option<Decimal>,
    pub submit_disabled: Vec<String>,
    pub cancel_disabled: Vec<String>,
    pub results: Vec<ManualResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ManualConfirm {
    pub exchange: Exchange,
    pub symbol: String,
    pub side: OrderSide,
    pub quantity: Decimal,
    pub qty_text: String,
    pub est_notional: Option<Decimal>,
    pub reduce_only: bool,
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

fn rounded(form: &ManualForm, snap: &UiSnapshot) -> Result<(Decimal, String), String> {
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
    match Quantity::round_down(raw, &rules.lot) {
        Ok(q) => Ok((q.value(), format!("{} {}", q.to_order_string(&rules.lot), format::base_coin(&symbol)))),
        Err(tong_funding_core::quantity::QuantityError::BelowMinimum { .. }) => Err("低於最小下單量".into()),
        Err(e) => Err(e.to_string()),
    }
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
    let r = rounded(form, snap);
    let price = snap.market.get(&form.exchange).and_then(|f| f.observations.iter().find(|o| o.symbol == form.symbol.trim().to_ascii_uppercase())).map(|o| o.mark_price);
    let est_notional = r.as_ref().ok().zip(price).map(|((q, _), p)| *q * p);

    let mut submit_disabled = Vec::new();
    match &snap.engine {
        None => submit_disabled.push("引擎未啟動".into()),
        Some(e) => {
            if !form.reduce_only {
                submit_disabled.extend(refusing_blockers(e).iter().map(blocker_text));
            }
        }
    }
    if !TRADABLE_EXCHANGES.contains(&form.exchange) {
        submit_disabled.push("OKX 不提供下單".into());
    } else if let Err(why) = panel_state(snap, form.exchange) {
        submit_disabled.push(why);
    }
    if let Err(why) = &r {
        submit_disabled.push(why.clone());
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

    ManualVm { panels, mode, env_text: env_text(mode), rounded: r.map(|(q, _)| q), est_notional, submit_disabled, cancel_disabled, results: results(snap) }
}

/// Opens the confirmation; `None` while submit is disabled. Sends nothing.
pub fn open_confirm(vm: &ManualVm, form: &ManualForm) -> Option<ManualConfirm> {
    if !vm.submit_disabled.is_empty() {
        return None;
    }
    let mode = vm.mode?;
    let quantity = *vm.rounded.as_ref().ok()?;
    let symbol = form.symbol.trim().to_ascii_uppercase();
    let qty_text = format!("{} {}", quantity.normalize(), format::base_coin(&symbol));
    Some(ManualConfirm {
        exchange: form.exchange,
        symbol,
        side: form.side,
        quantity,
        qty_text,
        est_notional: vm.est_notional,
        reduce_only: form.reduce_only,
        mode,
        env_text: env_text(Some(mode)),
        warning: "單腿下單不會自動建立對腿",
    })
}

/// After the confirmation: exactly one `ManualOrder` command (the engine's single order path).
pub fn confirm(c: &ManualConfirm, sink: &dyn CommandSink) {
    sink.send(
        format!("手動下單 {} {} {:?} {}", c.exchange.name(), c.symbol, c.side, c.qty_text),
        Command::ManualOrder(ManualOrder { exchange: c.exchange, symbol: c.symbol.clone(), side: c.side, quantity: c.quantity, reduce_only: c.reduce_only }),
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
    /// Signed: long positive, short negative.
    pub quantity: Decimal,
    pub is_long: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderRow {
    pub exchange: Exchange,
    pub symbol: String,
    pub order_id: Option<String>,
    pub remaining: Decimal,
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
        (!p.quantity.is_zero()).then(|| PositionRow { exchange: p.exchange, symbol: p.symbol.clone(), quantity: p.quantity, is_long: p.quantity > Decimal::ZERO })
    })
}

/// Open orders per tradable exchange; rows without an id are kept (shown, not pickable).
pub fn open_orders(snap: &UiSnapshot) -> Vec<PickList<OrderRow>> {
    pick_lists(snap, |a| &a.open_orders, |o| {
        let order_id = o.client_order_id.as_ref().map(|i| i.trim().to_string()).filter(|i| !i.is_empty());
        Some(OrderRow { exchange: o.exchange, symbol: o.symbol.clone(), order_id, remaining: o.remaining_quantity })
    })
}

/// Close = opposite side, the whole position, reduce-only. Quantity is passed unrounded; `build`
/// floors it to the exchange step and the confirmation shows the result.
pub fn close_prefill(p: &AccountPosition) -> ManualPrefill {
    ManualPrefill {
        exchange: p.exchange,
        symbol: p.symbol.clone(),
        side: if p.quantity > Decimal::ZERO { OrderSide::Sell } else { OrderSide::Buy },
        quantity: p.quantity.abs().normalize().to_string(),
        reduce_only: true,
    }
}

/// `None` for an order without an id (it cannot be cancelled from here).
pub fn cancel_prefill(o: &OrderRow) -> Option<CancelPrefill> {
    o.order_id.clone().map(|order_id| CancelPrefill { exchange: o.exchange, symbol: o.symbol.clone(), order_id })
}

#[cfg(test)]
#[path = "manual_order_tests.rs"]
mod tests;
