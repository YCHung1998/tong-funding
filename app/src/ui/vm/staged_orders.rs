//! Staged orders view-model (ui-trading-pages tasks 1.1–1.3; spec staged-orders-page). Pure: the
//! page renders this and sends engine commands through [`CommandSink`]; it never decides
//! pre-trade checks (the engine does) and never offers an automatic remedy.
//!
//! - one-click submit = one `EnterSelected` with every selected PREPARED pair, only after the
//!   per-leg confirmation built here (design D4: what is listed is what is sent); allowed in AUTO
//!   too: the engine refuses a pair the scheduler already took (land-then-act).
//! - disabled reasons are an enum (design D5); "設定不完整" comes from `RiskConfig::missing_fields`
//!   only (D6, via `risk_settings::missing_display`).

use std::collections::BTreeSet;

use serde_json::Value;
use tong_funding_core::pair::PairState;
use tong_funding_core::pnl::{Component, PnlStatus};
use tong_funding_core::risk::{effective_for_pair, ExecutionMode, TriggerMode};
use tong_funding_core::trade_cost::{estimate_cost, CostEstimate, CostInput};
use tong_funding_core::types::{Decimal, Exchange};

use super::bridge::{tradable_exchanges, CommandSink, LegAccount, UiSnapshot};
use super::contract_settings::{pair_quantity, QuoteCell};
use super::leverage_cap::{self, CapCheck};
use super::positions::{spread_view, SpreadView};
use super::engine_view::{blocker_text, refusing_blockers};
use super::labels::{block_reason_label, execution_mode_text, failed_checks_text, pair_state_label, pair_state_plain, pair_state_text, plain_error, trigger_mode_text, DisabledNote};
use super::env_label::{ConfirmKind, EnvLabel};
use super::risk_settings::missing_display;
use crate::engine::command::{Blocker, Command, PairView};
use crate::engine::ports::OrderSide;
use crate::engine::timings::EngineTimings;
use crate::store::event_query::StoredEvent;

/// One PREPARED pair.
#[derive(Debug, Clone, PartialEq)]
pub struct StagedRow {
    pub uuid: String,
    pub pair_id: String,
    pub symbol: String,
    pub long: Exchange,
    pub short: Exchange,
    pub gross_spread: Option<Decimal>,
    pub net_edge_pct: Option<Decimal>,
    pub notional: Option<Decimal>,
    pub leverage: Option<Decimal>,
    pub margin: Option<Decimal>,
    pub long_qty: QuoteCell,
    pub short_qty: QuoteCell,
    /// Estimated cost at the shared quantity (trade-cost-estimate), recomputed with every snapshot.
    pub cost: CostView,
    /// symbol-leverage-cap: both legs' caps and whether the leverage fits them (needs notional + leverage).
    pub cap: Option<CapCheck>,
    /// `Err(reason)` = the checkbox is disabled.
    pub selectable: Result<(), String>,
    pub selected: bool,
    /// Time until the entry trigger (`T − entry_lead`), if still ahead.
    pub entry_in_ms: Option<i64>,
    /// live-banners: `LIVE` in a Live process.
    pub badge: Option<&'static str>,
}

/// The cost estimate of one pair, or why there is none (no number is shown without its inputs).
#[derive(Debug, Clone, PartialEq)]
pub enum CostView {
    Unavailable(String),
    Estimate(CostDisplay),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CostDisplay {
    pub long: Exchange,
    pub short: Exchange,
    pub qty: Decimal,
    /// Best ask of the long leg's exchange / best bid of the short leg's exchange.
    pub long_ask: Decimal,
    pub short_bid: Decimal,
    pub estimate: CostEstimate,
    /// The OLDER of the two quotes' receive times (the number is only as fresh as this).
    pub quote_at: i64,
    pub quote_age_ms: i64,
    /// "會吃到第二檔" hints and quote-refresh failures; never block anything.
    pub warnings: Vec<String>,
}

impl CostView {
    /// The text lines the page shows.
    pub fn lines(&self) -> Vec<String> {
        match self {
            CostView::Unavailable(why) => vec![format!("預估成本：{why}")],
            CostView::Estimate(c) => {
                let e = &c.estimate;
                let leg = |tag: &str, ex: Exchange, side: &str, px: Decimal, l: &tong_funding_core::trade_cost::LegEstimate| {
                    format!(
                        "{tag} {} {side} {} → 價值 {} · 開倉費 {} · 保證金 {}",
                        ex.name(),
                        px.normalize(),
                        super::format::money(l.cost.value, 2),
                        super::format::fixed(l.cost.open_fee, 4),
                        super::format::money(l.cost.margin, 2)
                    )
                };
                let mut out = vec![
                    leg("L", c.long, "賣一", c.long_ask, &e.long),
                    leg("S", c.short, "買一", c.short_bid, &e.short),
                    format!(
                        "合計保證金 {} USDT · 手續費（開倉＋預估平倉）{} USDT",
                        super::format::money(e.total_margin, 2),
                        super::format::money(e.total_fees, 2)
                    ),
                    format!("報價 {} UTC（{} 秒前）", super::format::utc_hms(c.quote_at), super::format::secs(c.quote_age_ms)),
                ];
                out.extend(c.warnings.iter().cloned());
                out
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub pairs: usize,
    pub legs: usize,
    pub notional: Decimal,
    pub margin: Decimal,
}

/// Why one-click submit is disabled (design D5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisabledReason {
    NothingSelected,
    ConfigIncomplete(Vec<String>),
    KillSwitchOn,
    Halted(String),
    /// Selected pairs that are no longer PREPARED (symbols).
    NotPrepared(Vec<String>),
    EngineUnavailable,
}

impl DisabledReason {
    pub fn text(&self) -> String {
        match self {
            DisabledReason::NothingSelected => "尚未選取配對".into(),
            DisabledReason::ConfigIncomplete(m) => format!("設定不完整：{}", m.join("、")),
            DisabledReason::KillSwitchOn => "緊急停止中".into(),
            DisabledReason::Halted(r) => r.clone(),
            DisabledReason::NotPrepared(s) => format!("已不是「{}」狀態：{}", pair_state_plain(PairState::Prepared).0, s.join("、")),
            DisabledReason::EngineUnavailable => "引擎未啟動".into(),
        }
    }
}

/// One leg in the confirmation (same data the command is built from).
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmLeg {
    pub uuid: String,
    pub exchange: Exchange,
    pub symbol: String,
    pub side: OrderSide,
    pub qty_text: String,
    pub notional: Decimal,
    pub leverage: Decimal,
    pub margin: Decimal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingConfirm {
    pub pairs: Vec<String>,
    pub legs: Vec<ConfirmLeg>,
    pub mode: ExecutionMode,
    pub env: EnvLabel,
    pub env_text: String,
}

impl PendingConfirm {
    /// The environment and the total amount of every leg listed, repeated in the confirmation.
    pub fn env_line(&self) -> String {
        self.env.confirm_env_line(Some(self.legs.iter().map(|l| l.notional).sum()))
    }

    pub fn button(&self) -> &'static str {
        self.env.confirm_button(ConfirmKind::Enter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmOutcome {
    pub sent: Vec<String>,
    /// Symbols of pairs that left PREPARED while the confirmation was open.
    pub excluded: Vec<String>,
}

/// One leg of an execution attempt (history).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegStatus {
    Accepted,
    Rejected(String),
    Unknown(String),
    NotSent,
}

impl LegStatus {
    pub fn text(&self) -> String {
        match self {
            LegStatus::Accepted => "成功".into(),
            LegStatus::Rejected(r) => format!("失敗：{r}"),
            LegStatus::Unknown(r) => format!("結果未知：{r}"),
            LegStatus::NotSent => "未送出".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttemptLeg {
    pub leg: &'static str,
    pub exchange: String,
    pub side: OrderSide,
    pub status: LegStatus,
    pub order_id_text: String,
}

impl AttemptLeg {
    /// `long · Binance · BUY · 成功`: the primary text of a leg (the order id is secondary, see [`order_text`](Self::order_text)).
    pub fn summary(&self) -> String {
        let side = match self.side {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
        };
        format!("{} · {} · {side} · {}", self.leg, self.exchange, self.status.text())
    }

    /// `訂單 8389765` (a simulated leg: `訂單 模擬 · <client id>`); `None` when no order was sent.
    pub fn order_text(&self) -> Option<String> {
        (self.order_id_text != NO_ORDER).then(|| format!("訂單 {}", self.order_id_text))
    }
}

const NO_ORDER: &str = "—";
/// Shown where neither the engine, the store nor an order event names the symbol / an exchange: never blank.
pub const UNKNOWN_SYMBOL: &str = "（幣種未知）";
pub const UNKNOWN_EXCHANGE: &str = "（交易所未知）";

/// The latest entry attempt of one pair, rebuilt from stored events (survives restarts).
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptView {
    pub uuid: String,
    pub symbol: String,
    pub at: i64,
    pub simulated: bool,
    pub mode_label: &'static str,
    pub mode_text: String,
    pub legs: Vec<AttemptLeg>,
    pub state: String,
    pub state_text: String,
    pub failed_checks: Vec<String>,
    pub block_reason: Option<String>,
    pub needs_manual: bool,
    pub when: String,
    pub headline: String,
    pub codes: String,
    pub net_pnl: Option<String>,
}

/// A leg to close by hand (reduce-only; quantity = the account's actual position).
#[derive(Debug, Clone, PartialEq)]
pub struct CloseLeg {
    pub exchange: Exchange,
    pub symbol: String,
    pub quantity: Decimal,
}

/// Manual handling of a pair in PARTIAL_FAILURE / IMBALANCED / UNRESOLVED.
#[derive(Debug, Clone, PartialEq)]
pub struct ManualHandling {
    pub uuid: String,
    pub symbol: String,
    pub state: PairState,
    pub simulated: bool,
    /// The world of this pair (live-banners) and its two legs' notional from the scan snapshot.
    pub env: EnvLabel,
    pub notional: Option<Decimal>,
    /// Per leg: exchange and the latest position read (signed).
    pub legs: Vec<(Exchange, Result<Decimal, String>)>,
    /// "人工要求平倉": the non-flat legs, or why the positions are not known.
    pub close: Result<Vec<CloseLeg>, String>,
    /// "人工確認已平倉": enabled only when the latest read shows both legs flat with no open order.
    pub confirm_closed: Result<(), String>,
}

impl ManualHandling {
    /// `BTCUSDT · 單腿失敗（PARTIAL_FAILURE） · Demo 帳戶（EXCHANGE_DEMO）`.
    pub fn heading(&self) -> String {
        format!("{} · {} · {}", self.symbol, pair_state_text(self.state.as_str()), execution_mode_text(self.env.identity, self.env.mode))
    }


    /// The only two actions (no automatic remedy exists).
    pub fn actions(&self) -> [&'static str; 2] {
        ["人工要求平倉", "人工確認已平倉"]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CloseConfirm {
    pub uuid: String,
    pub symbol: String,
    pub legs: Vec<CloseLeg>,
    pub env: EnvLabel,
    /// Both legs' notional from the scan snapshot, when known.
    pub notional: Option<Decimal>,
}

impl CloseConfirm {
    pub fn env_line(&self) -> String {
        self.env.confirm_env_line(self.notional)
    }

    pub fn button(&self) -> &'static str {
        self.env.confirm_button(ConfirmKind::Close)
    }
}

/// A RECONCILED pair (exit countdown / close now).
#[derive(Debug, Clone, PartialEq)]
pub struct RunningRow {
    pub uuid: String,
    pub symbol: String,
    pub exit_in_ms: i64,
    /// MANUAL: "立即平倉" is offered (with its own confirmation).
    pub close_now: bool,
    pub env: EnvLabel,
    /// Both legs' notional from the scan snapshot, when known.
    pub notional: Option<Decimal>,
    /// spread-position-tag: live positive / negative / neutral tag; `None` = not shown (price missing / stale).
    pub spread: Option<SpreadView>,
}

impl RunningRow {
    pub fn env_line(&self) -> String {
        self.env.confirm_env_line(self.notional)
    }

    pub fn button(&self) -> &'static str {
        self.env.confirm_button(ConfirmKind::Close)
    }

    pub fn action_text(&self) -> &'static str {
        if self.close_now { "立即平倉" } else { "自動" }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StagedVm {
    pub rows: Vec<StagedRow>,
    pub staged_count: usize,
    pub selected_count: usize,
    pub summary: Summary,
    pub disabled: Vec<DisabledReason>,
    pub mode: Option<ExecutionMode>,
    pub identity: crate::identity::ProcessIdentity,
    pub trigger_mode: Option<TriggerMode>,
    /// Available margin text of each ENABLED exchange (never 0 for unknown).
    pub margins: Vec<(Exchange, String)>,
    /// Exchanges the risk settings left unticked: one muted line each, their margin text behind it.
    pub disabled_exchanges: Vec<DisabledNote>,
    pub last_results: Vec<AttemptView>,
    pub manual: Vec<ManualHandling>,
    pub running: Vec<RunningRow>,
}

impl StagedVm {
    /// The execution world in plain words with the code after it (`Demo 帳戶（EXCHANGE_DEMO）`).
    pub fn mode_text(&self) -> String {
        self.mode.map_or_else(|| "未知".to_string(), |m| execution_mode_text(self.identity, m))
    }

    /// The trigger mode in plain words with the code after it (`自動（AUTO）`).
    pub fn trigger_text(&self) -> String {
        self.trigger_mode.map_or_else(|| "未知".to_string(), trigger_mode_text)
    }


    pub fn mode_label(&self) -> &'static str {
        match self.mode {
            Some(m) => EnvLabel::of(self.identity, m).env_name(),
            None => "未知",
        }
    }
}

fn dec_of(v: &Value, k: &str) -> Option<Decimal> {
    match v.get(k)? {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.to_string().parse().ok(),
        _ => None,
    }
}

fn price_of(snap: &UiSnapshot, ex: Exchange, symbol: &str) -> Option<Decimal> {
    snap.market.get(&ex)?.observations.iter().find(|o| o.symbol == symbol).map(|o| o.mark_price)
}

/// The shared base-coin quantity of a pair (the same number `plan_submit` will send).
fn base_qty_of(c: &QuoteCell) -> Option<Decimal> {
    match c {
        QuoteCell::Qty { qty, .. } => Some(*qty),
        QuoteCell::Contracts { base, .. } => Some(*base),
        _ => None,
    }
}

/// Estimate at the shared quantity: long leg buys at the best ask, short leg sells at the best bid.
/// Uses only the snapshot (quotes, fees, leverage, quantity): no request, background-computed.
fn cost_of(snap: &UiSnapshot, p: &PairView, leverage: Option<Decimal>, long_qty: &QuoteCell, short_qty: &QuoteCell, now_ms: i64) -> CostView {
    let no = |why: String| CostView::Unavailable(why);
    let (Some(qty_l), Some(qty_s)) = (base_qty_of(long_qty), base_qty_of(short_qty)) else {
        let why = if base_qty_of(long_qty).is_none() { long_qty.text() } else { short_qty.text() };
        return no(format!("無共同數量（{why}）"));
    };
    let Some(leverage) = leverage else { return no("槓桿未知".into()) };
    let eff = effective_for_pair(&snap.settings.risk, &snap.settings.overrides, p.long_exchange, p.short_exchange);
    let unset: Vec<&str> = [(eff.long_taker_fee_pct, p.long_exchange), (eff.short_taker_fee_pct, p.short_exchange)]
        .into_iter()
        .filter_map(|(f, ex)| f.is_none().then_some(ex.name()))
        .collect();
    let (Some(fee_l), Some(fee_s)) = (eff.long_taker_fee_pct, eff.short_taker_fee_pct) else {
        return no(format!("未設定手續費率（{}）", unset.join("、")));
    };
    let book = |ex: Exchange| snap.books.get(&ex).and_then(|f| f.books.get(&p.symbol));
    let (Some(bl), Some(bs)) = (book(p.long_exchange), book(p.short_exchange)) else {
        let missing: Vec<&str> = [(book(p.long_exchange).is_none(), p.long_exchange), (book(p.short_exchange).is_none(), p.short_exchange)]
            .into_iter()
            .filter_map(|(m, ex)| m.then_some(ex.name()))
            .collect();
        return no(format!("無報價（{}）", missing.join("、")));
    };
    // Both legs are sized to one quantity; a mismatch would mean the two cells disagree.
    debug_assert_eq!(qty_l, qty_s);
    let input = CostInput {
        qty: qty_l,
        long_ask: bl.ask_price,
        long_ask_qty: bl.ask_qty,
        short_bid: bs.bid_price,
        short_bid_qty: bs.bid_qty,
        long_fee_pct: fee_l,
        short_fee_pct: fee_s,
        leverage,
    };
    let estimate = match estimate_cost(&input) {
        Ok(e) => e,
        Err(e) => return no(format!("無法計算（{e}）")),
    };
    let mut warnings = Vec::new();
    for (ex, side, est, top_qty) in
        [(p.long_exchange, "多腿", estimate.long, bl.ask_qty), (p.short_exchange, "空腿", estimate.short, bs.bid_qty)]
    {
        if est.exceeds_top_level {
            warnings.push(format!(
                "{} {side}數量 {} 大於一檔掛單量 {}，會吃到第二檔，實際價格會更差",
                ex.name(),
                qty_l.normalize(),
                top_qty.normalize()
            ));
        }
    }
    for ex in [p.long_exchange, p.short_exchange] {
        if let Some((why, _)) = snap.books.get(&ex).and_then(|f| f.last_error.as_ref()) {
            warnings.push(format!("{} 報價更新失敗（{why}），顯示的是舊報價", ex.name()));
        }
    }
    let quote_at = bl.observed_at.min(bs.observed_at);
    CostView::Estimate(CostDisplay {
        long: p.long_exchange,
        short: p.short_exchange,
        qty: qty_l,
        long_ask: bl.ask_price,
        short_bid: bs.bid_price,
        estimate,
        quote_at,
        quote_age_ms: (now_ms - quote_at).max(0),
        warnings,
    })
}

/// Both legs' notional of a pair, from its scan snapshot.
fn pair_notional(snap: &UiSnapshot, uuid: &str) -> Option<Decimal> {
    snap.pair_entries.get(uuid).and_then(|e| dec_of(e, "notional_usdt")).map(|n| n * Decimal::from(2))
}

fn row_of(snap: &UiSnapshot, p: &PairView, selection: &BTreeSet<String>, now_ms: i64) -> StagedRow {
    let empty = Value::Null;
    let entry = snap.pair_entries.get(&p.internal_uuid).unwrap_or(&empty);
    let notional = dec_of(entry, "notional_usdt");
    let leverage = dec_of(entry, "leverage").filter(|l| *l > Decimal::ZERO);
    // matched-leg-quantity: both legs at one shared quantity, the notional being a cap.
    let rules = |ex: Exchange| snap.rules.get(&(ex, p.symbol.clone()));
    let (long_qty, short_qty) = match (notional, price_of(snap, p.long_exchange, &p.symbol), price_of(snap, p.short_exchange, &p.symbol)) {
        (Some(n), Some(pl), Some(ps)) => {
            pair_quantity(&p.symbol, n, (p.long_exchange, pl, rules(p.long_exchange)), (p.short_exchange, ps, rules(p.short_exchange)))
        }
        (None, _, _) => (QuoteCell::NoRules("掃描快照缺少 notional".into()), QuoteCell::NoRules("掃描快照缺少 notional".into())),
        (_, _, _) => (QuoteCell::NoPrice, QuoteCell::NoPrice),
    };
    let cap = notional.zip(leverage).map(|(n, l)| leverage_cap::check(snap, p.long_exchange, p.short_exchange, &p.symbol, n, l, now_ms));
    let mode = snap.engine.as_ref().map(|e| e.execution_mode);
    let selectable = if p.state != PairState::Prepared {
        Err(format!("狀態 {}", pair_state_text(p.state.as_str())))
    } else if matches!(long_qty, QuoteCell::BelowMinimum) || matches!(short_qty, QuoteCell::BelowMinimum) {
        Err("低於最小下單量".into())
    } else if let Some(why) = cap.as_ref().and_then(|c| c.blocked_reason(mode)) {
        Err(why)
    } else if long_qty.order_qty().is_none() || short_qty.order_qty().is_none() {
        // Conservative: a leg whose quantity cannot be shown cannot be confirmed.
        Err(format!("數量未知（{}）", if long_qty.order_qty().is_none() { long_qty.text() } else { short_qty.text() }))
    } else {
        Ok(())
    };
    let entry_at = p.settlement_ms - EngineTimings::default().entry_lead_ms;
    StagedRow {
        uuid: p.internal_uuid.clone(),
        pair_id: p.pair_id.clone(),
        symbol: p.symbol.clone(),
        long: p.long_exchange,
        short: p.short_exchange,
        gross_spread: dec_of(entry, "gross_spread"),
        net_edge_pct: dec_of(entry, "net_edge_pct"),
        notional,
        leverage,
        margin: notional.zip(leverage).map(|(n, l)| n / l),
        selected: selectable.is_ok() && selection.contains(&p.internal_uuid),
        selectable,
        cost: cost_of(snap, p, leverage, &long_qty, &short_qty, now_ms),
        long_qty,
        short_qty,
        cap,
        entry_in_ms: (entry_at > now_ms).then_some(entry_at - now_ms),
        badge: EnvLabel::from_snapshot(snap).badge(),
    }
}

fn is_alert(s: PairState) -> bool {
    matches!(s, PairState::PartialFailure | PairState::Imbalanced | PairState::Unresolved)
}

fn margin_text(acc: Option<&LegAccount>) -> String {
    match acc {
        None => "未知（尚未查詢）".into(),
        Some(a) => match &a.available_margin {
            Ok(m) => format!("{} USDT", super::format::money(*m, 2)),
            Err(e) => format!("未知（{}）", plain_error(e)),
        },
    }
}

/// Builds the page. `selection` holds internal uuids; rows that cannot be selected are ignored.
pub fn build(snap: &UiSnapshot, selection: &BTreeSet<String>, now_ms: i64) -> StagedVm {
    let engine = snap.engine.as_ref();
    let pairs: Vec<&PairView> = engine.map(|e| e.pairs.iter().collect()).unwrap_or_default();
    let rows: Vec<StagedRow> = pairs.iter().filter(|p| p.state == PairState::Prepared).map(|p| row_of(snap, p, selection, now_ms)).collect();
    let selected: Vec<&StagedRow> = rows.iter().filter(|r| r.selected).collect();
    let two = Decimal::TWO;
    let summary = Summary {
        pairs: selected.len(),
        legs: selected.len() * 2,
        notional: selected.iter().filter_map(|r| r.notional).map(|n| n * two).sum(),
        margin: selected.iter().filter_map(|r| r.margin).map(|m| m * two).sum(),
    };

    let mut disabled = Vec::new();
    match engine {
        None => disabled.push(DisabledReason::EngineUnavailable),
        Some(e) => {
            // Selected uuids whose pair is no longer PREPARED (taken by the scheduler, cancelled...).
            let gone: Vec<String> =
                selection.iter().filter_map(|u| e.pair(u)).filter(|p| p.state != PairState::Prepared).map(|p| p.symbol.clone()).collect();
            if selected.is_empty() && gone.is_empty() {
                disabled.push(DisabledReason::NothingSelected);
            }
            if let Some(err) = &snap.settings.error {
                disabled.push(DisabledReason::ConfigIncomplete(vec![format!("風控設定讀取失敗（{err}）")]));
            } else {
                let missing = missing_display(&snap.settings.risk);
                if !missing.is_empty() {
                    disabled.push(DisabledReason::ConfigIncomplete(missing));
                }
            }
            for b in refusing_blockers(e) {
                disabled.push(match b {
                    Blocker::KillSwitch => DisabledReason::KillSwitchOn,
                    Blocker::KillSwitchUnreadable(_) | Blocker::StoreHalted(_) | Blocker::ReconciliationPending(_) => DisabledReason::Halted(blocker_text(&b)),
                });
            }
            if !gone.is_empty() {
                disabled.push(DisabledReason::NotPrepared(gone));
            }
        }
    }

    // ui-plain-language: an exchange the risk settings left unticked has no margin line, only its muted line.
    let (mut margins, mut disabled_exchanges) = (Vec::new(), Vec::new());
    for ex in tradable_exchanges(snap.identity).iter().copied() {
        let text = margin_text(snap.leg_accounts.get(&(false, ex)));
        if snap.exchange_enabled(ex) {
            margins.push((ex, text));
        } else {
            disabled_exchanges.push(DisabledNote::new(ex, Some(format!("可用保證金：{text}"))));
        }
    }
    let manual = pairs.iter().filter(|p| is_alert(p.state)).map(|p| handling(snap, p)).collect();
    let exit_delay = EngineTimings::default().exit_delay_ms;
    let trigger_mode = engine.map(|e| e.trigger_mode);
    let running = pairs
        .iter()
        .filter(|p| p.state == PairState::Reconciled)
        .map(|p| RunningRow {
            env: EnvLabel::of_pair(snap.identity, p.simulated),
            notional: pair_notional(snap, &p.internal_uuid),
            uuid: p.internal_uuid.clone(),
            symbol: p.symbol.clone(),
            exit_in_ms: p.settlement_ms + exit_delay - now_ms,
            close_now: trigger_mode == Some(TriggerMode::Manual),
            spread: spread_view(snap, p.long_exchange, p.short_exchange, &p.symbol, now_ms),
        })
        .collect();
    StagedVm {
        staged_count: rows.len(),
        selected_count: selected.len(),
        rows,
        summary,
        disabled,
        mode: engine.map(|e| e.execution_mode),
        identity: snap.identity,
        trigger_mode,
        margins,
        disabled_exchanges,
        last_results: last_results(snap, 5),
        manual,
        running,
    }
}

/// `已排除（已不是「已暫存」狀態）：BTCUSDT`: pairs that left PREPARED while the confirmation was open.
pub fn excluded_note(symbols: &[String]) -> String {
    format!("已排除（已不是「{}」狀態）：{}", pair_state_plain(PairState::Prepared).0, symbols.join("、"))
}

/// `進行中（持倉中）`: the title of the RECONCILED block.
pub fn running_title() -> String {
    format!("進行中（{}）", pair_state_plain(PairState::Reconciled).0)
}

/// "全選": every selectable row.
pub fn select_all(vm: &StagedVm) -> BTreeSet<String> {
    vm.rows.iter().filter(|r| r.selectable.is_ok()).map(|r| r.uuid.clone()).collect()
}

/// "全不選".
pub fn select_none() -> BTreeSet<String> {
    BTreeSet::new()
}

fn confirm_legs(r: &StagedRow) -> Option<[ConfirmLeg; 2]> {
    let (notional, leverage, margin) = (r.notional?, r.leverage?, r.margin?);
    let leg = |exchange, side, q: &QuoteCell| ConfirmLeg { uuid: r.uuid.clone(), exchange, symbol: r.symbol.clone(), side, qty_text: q.text(), notional, leverage, margin };
    Some([leg(r.long, OrderSide::Buy, &r.long_qty), leg(r.short, OrderSide::Sell, &r.short_qty)])
}

/// Opens the per-leg confirmation; `None` while submit is disabled. Sends nothing.
pub fn open_confirm(vm: &StagedVm) -> Option<PendingConfirm> {
    if !vm.disabled.is_empty() {
        return None;
    }
    let mode = vm.mode?;
    let mut legs = Vec::new();
    let mut pairs = Vec::new();
    for r in vm.rows.iter().filter(|r| r.selected) {
        legs.extend(confirm_legs(r)?);
        pairs.push(r.uuid.clone());
    }
    let env = EnvLabel::of(vm.identity, mode);
    (!pairs.is_empty()).then(|| PendingConfirm { pairs, legs, mode, env, env_text: env.staged_env_text().into() })
}

/// The user confirmed: pairs that left PREPARED meanwhile are excluded; the rest go in ONE command.
pub fn confirm(p: &PendingConfirm, snap: &UiSnapshot, sink: &dyn CommandSink) -> ConfirmOutcome {
    let mut sent = Vec::new();
    let mut excluded = Vec::new();
    for uuid in &p.pairs {
        match snap.engine.as_ref().and_then(|e| e.pair(uuid)) {
            Some(v) if v.state == PairState::Prepared => sent.push(uuid.clone()),
            Some(v) => excluded.push(v.symbol.clone()),
            None => excluded.push(p.legs.iter().find(|l| &l.uuid == uuid).map(|l| l.symbol.clone()).unwrap_or_else(|| uuid.clone())),
        }
    }
    if !sent.is_empty() {
        sink.send(format!("一鍵送出 {} 筆", sent.len()), Command::EnterSelected { pairs: sent.clone() });
    }
    ConfirmOutcome { sent, excluded }
}

/// Flip `trigger_mode` (the engine persists it and writes `TRIGGER_MODE_CHANGED`).
pub fn toggle_trigger_mode(vm: &StagedVm, sink: &dyn CommandSink) -> bool {
    let Some(current) = vm.trigger_mode else { return false };
    let target = match current {
        TriggerMode::Auto => TriggerMode::Manual,
        TriggerMode::Manual => TriggerMode::Auto,
    };
    sink.send(format!("trigger_mode → {target:?}"), Command::SetTriggerMode(target));
    true
}

/// "立即平倉" of a RECONCILED pair (MANUAL only; after its confirmation).
pub fn send_close_now(r: &RunningRow, sink: &dyn CommandSink) -> bool {
    if !r.close_now {
        return false;
    }
    sink.send(format!("立即平倉 {}", r.symbol), Command::ManualExit { pair: r.uuid.clone() });
    true
}

// ---- 1.3 manual handling --------------------------------------------------------------------

fn leg_read(snap: &UiSnapshot, simulated: bool, ex: Exchange, symbol: &str) -> Result<(Decimal, usize), String> {
    let name = ex.name();
    let acc = snap.leg_accounts.get(&(simulated, ex)).ok_or_else(|| format!("{name} 尚未查詢持倉"))?;
    let positions = acc.positions.as_ref().map_err(|e| format!("{name} 持倉查詢失敗：{e}"))?;
    if !positions.complete {
        return Err(format!("{name} 持倉清單不完整"));
    }
    let orders = acc.open_orders.as_ref().map_err(|e| format!("{name} 委託查詢失敗：{e}"))?;
    if !orders.complete {
        return Err(format!("{name} 委託清單不完整"));
    }
    let qty = positions.items.iter().filter(|p| p.symbol == symbol).map(|p| p.quantity).sum();
    Ok((qty, orders.items.iter().filter(|o| o.symbol == symbol).count()))
}

fn handling(snap: &UiSnapshot, p: &PairView) -> ManualHandling {
    let reads: Vec<(Exchange, Result<(Decimal, usize), String>)> =
        [p.long_exchange, p.short_exchange].into_iter().map(|ex| (ex, leg_read(snap, p.simulated, ex, &p.symbol))).collect();
    let errors: Vec<String> = reads.iter().filter_map(|(_, r)| r.as_ref().err().cloned()).collect();
    let close = if errors.is_empty() {
        Ok(reads
            .iter()
            .filter_map(|(ex, r)| r.as_ref().ok().filter(|(q, _)| !q.is_zero()).map(|(q, _)| CloseLeg { exchange: *ex, symbol: p.symbol.clone(), quantity: q.abs() }))
            .collect())
    } else {
        Err(errors.join("；"))
    };
    let confirm_closed = if !errors.is_empty() {
        Err(errors.join("；"))
    } else {
        let mut why = Vec::new();
        for (ex, r) in &reads {
            if let Ok((q, open)) = r {
                if !q.is_zero() {
                    why.push(format!("{} 仍有持倉", ex.name()));
                }
                if *open > 0 {
                    why.push(format!("{} 有未成交委託", ex.name()));
                }
            }
        }
        if why.is_empty() { Ok(()) } else { Err(why.join("；")) }
    };
    ManualHandling {
        uuid: p.internal_uuid.clone(),
        symbol: p.symbol.clone(),
        state: p.state,
        simulated: p.simulated,
        env: EnvLabel::of_pair(snap.identity, p.simulated),
        notional: pair_notional(snap, &p.internal_uuid),
        legs: reads.into_iter().map(|(ex, r)| (ex, r.map(|(q, _)| q))).collect(),
        close,
        confirm_closed,
    }
}

/// Opens the "人工要求平倉" confirmation (the legs and actual positions); sends nothing.
pub fn request_close(h: &ManualHandling) -> Option<CloseConfirm> {
    h.close.as_ref().ok().map(|legs| CloseConfirm { uuid: h.uuid.clone(), symbol: h.symbol.clone(), legs: legs.clone(), env: h.env, notional: h.notional })
}

/// After its confirmation: the engine re-reads positions and closes reduce-only.
pub fn send_close(c: &CloseConfirm, sink: &dyn CommandSink) {
    sink.send(format!("人工要求平倉 {}", c.symbol), Command::ManualClose { pair: c.uuid.clone() });
}

/// "人工確認已平倉" (the engine re-queries both legs before accepting).
pub fn send_confirm_closed(h: &ManualHandling, sink: &dyn CommandSink) -> bool {
    if h.confirm_closed.is_err() {
        return false;
    }
    sink.send(format!("人工確認已平倉 {}", h.symbol), Command::ConfirmClosed { pair: h.uuid.clone(), verified_flat: true });
    true
}

// ---- last execution results (from events) ---------------------------------------------------

fn payload(e: &StoredEvent) -> Value {
    serde_json::from_str(&e.payload).unwrap_or(Value::Null)
}

fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// `淨損益 +1.23 USDT` of a finished (FINALIZED) real pair: the net of its newest PnL event
/// (`breakdown.total.net`). `—` when that net cannot be computed, `（PnL 不完整）` when the PnL is
/// INCOMPLETE; `None` when there is no PnL (not finished, simulated, not computed yet).
fn net_pnl_text(snap: &UiSnapshot, pair_id: Option<&str>, state: &str) -> Option<String> {
    if state != PairState::Finalized.as_str() {
        return None;
    }
    let f = snap.funding.get(pair_id?)?;
    if f.simulated {
        return None;
    }
    let p = f.pnl.as_ref()?;
    let b = p.breakdown.as_ref()?;
    let note = if p.status == PnlStatus::Complete.as_str() { "" } else { "（PnL 不完整）" };
    let net = if b.missing.contains(&Component::Net) { super::format::DASH.to_string() } else { format!("{} USDT", super::format::signed(b.total.net, 2)) };
    Some(format!("淨損益 {net}{note}"))
}

/// The latest entry attempt of up to `limit` pairs, newest first. An attempt starts at the
/// pair's PREPARED → PRE_TRADE_CHECK transition; legs come from its `ORDER_SUBMITTED` (open)
/// events; the state is the pair's latest transition. Never says "rolled back". The symbol and the
/// exchanges come from the pair (the engine's view, else the stored pair), so an attempt of a pair
/// the engine no longer holds is not blank; a BLOCKED attempt carries the reason from its transition.
pub fn last_results(snap: &UiSnapshot, limit: usize) -> Vec<AttemptView> {
    let mut events: Vec<&StoredEvent> = snap.trade_events.iter().collect();
    events.sort_by_key(|e| (e.ts_ms, e.id));
    let mut starts: Vec<(&str, i64, i64, bool)> = Vec::new(); // (pair, ts, id, simulated)
    for e in &events {
        let p = payload(e);
        if e.event_type == crate::engine::transition::PAIR_TRANSITION && str_of(&p, "to") == Some("PRE_TRADE_CHECK") {
            let Some(pair) = e.pair_id.as_deref() else { continue };
            let simulated = p.pointer("/detail/simulated").and_then(Value::as_bool).unwrap_or(true);
            starts.retain(|(q, ..)| *q != pair);
            starts.push((pair, e.ts_ms, e.id, simulated));
        }
    }
    starts.sort_by_key(|(_, ts, id, _)| std::cmp::Reverse((*ts, *id)));
    starts
        .into_iter()
        .take(limit)
        .map(|(pair, ts, id, simulated)| {
            let after = events.iter().filter(|e| e.pair_id.as_deref() == Some(pair) && (e.ts_ms, e.id) >= (ts, id));
            let view = snap.engine.as_ref().and_then(|e| e.pair(pair));
            let record = snap.pair_records.get(pair);
            let pair_id = view.map(|v| v.pair_id.clone()).or_else(|| record.map(|r| r.pair_id.clone()));
            let mut symbol = view.map(|v| v.symbol.clone()).or_else(|| record.map(|r| r.symbol.clone())).filter(|s| !s.is_empty());
            let mut state = view.map(|v| v.state.to_string()).or_else(|| record.map(|r| r.status.clone()));
            let exchange_of = |leg: usize| {
                view.map(|v| [v.long_exchange, v.short_exchange][leg]).or_else(|| record.and_then(|r| [r.long_exchange, r.short_exchange][leg])).map(|e| e.name().to_string()).unwrap_or_default()
            };
            let mut failed_checks = Vec::new();
            let mut block_reason = None;
            let mut legs: Vec<AttemptLeg> = vec![
                AttemptLeg { leg: "long", exchange: exchange_of(0), side: OrderSide::Buy, status: LegStatus::NotSent, order_id_text: NO_ORDER.into() },
                AttemptLeg { leg: "short", exchange: exchange_of(1), side: OrderSide::Sell, status: LegStatus::NotSent, order_id_text: NO_ORDER.into() },
            ];
            let mut last_to = None;
            for e in after {
                let p = payload(e);
                if e.event_type == crate::engine::transition::PAIR_TRANSITION {
                    last_to = str_of(&p, "to").map(str::to_string);
                    if let Some(list) = p.pointer("/detail/failed_checks").and_then(Value::as_array) {
                        failed_checks = list.iter().filter_map(Value::as_str).map(str::to_string).collect();
                    }
                    if str_of(&p, "to") == Some(PairState::Blocked.as_str()) {
                        block_reason = p.pointer("/detail/block").or_else(|| p.pointer("/detail/reason")).and_then(Value::as_str).map(str::to_string);
                    }
                } else if e.event_type == crate::engine::actor::ORDER_SUBMITTED && str_of(&p, "action") == Some("open") {
                    let idx = if str_of(&p, "leg") == Some("short") { 1 } else { 0 };
                    if let Some(s) = str_of(&p, "symbol") {
                        symbol = Some(s.to_string());
                    }
                    if let Some(x) = str_of(&p, "exchange") {
                        legs[idx].exchange = x.to_string();
                    }
                    let reason = str_of(&p, "reason").unwrap_or("").to_string();
                    legs[idx].status = match str_of(&p, "outcome") {
                        Some("accepted") => LegStatus::Accepted,
                        Some("rejected") => LegStatus::Rejected(reason),
                        _ => LegStatus::Unknown(reason),
                    };
                    let coid = str_of(&p, "client_order_id").unwrap_or("");
                    legs[idx].order_id_text = if simulated {
                        format!("模擬 · {coid}")
                    } else {
                        str_of(&p, "exchange_order_id").map(str::to_string).unwrap_or_else(|| "（交易所未回報）".into())
                    };
                }
            }
            if state.is_none() {
                state = last_to;
            }
            let state = state.unwrap_or_else(|| "未知".into());
            let symbol = symbol.unwrap_or_else(|| UNKNOWN_SYMBOL.to_string());
            for l in &mut legs {
                if l.exchange.is_empty() {
                    l.exchange = UNKNOWN_EXCHANGE.to_string();
                }
            }
            let alert_state = matches!(state.as_str(), "PARTIAL_FAILURE" | "IMBALANCED" | "UNRESOLVED");
            let leg_failed = legs.iter().any(|l| matches!(l.status, LegStatus::Rejected(_) | LegStatus::Unknown(_)));
            let needs_manual = alert_state || leg_failed;
            let env = EnvLabel::of_pair(snap.identity, simulated);
            let mode_label = env.env_name();
            let mode_text = execution_mode_text(snap.identity, if simulated { ExecutionMode::Simulation } else { ExecutionMode::ExchangeDemo });
            let state_text = pair_state_label(&state).0;
            let net_pnl = net_pnl_text(snap, pair_id.as_deref(), &state);
            let when = format!("{} UTC", super::format::utc_md_hms(ts));
            let is_blocked = state == PairState::Blocked.as_str() || !failed_checks.is_empty();
            let mut result = if needs_manual {
                format!("需人工處理（目前狀態：{state_text}）")
            } else if is_blocked {
                // every failed check by plain name; else the engine's own reason in plain words; else just the result
                let why = if !failed_checks.is_empty() {
                    Some(failed_checks_text(&failed_checks))
                } else {
                    block_reason.as_deref().map(block_reason_label)
                };
                match why {
                    Some(w) => format!("{}：{w}", pair_state_label(PairState::Blocked.as_str()).0),
                    None => pair_state_label(PairState::Blocked.as_str()).0,
                }
            } else {
                state_text.clone()
            };
            if let Some(n) = &net_pnl {
                result = format!("{result} · {n}");
            }
            // a real-money process names its world on the headline itself (live-banners); the others keep
            // it on the secondary line
            if snap.identity.is_live() {
                result = format!("{result} · {mode_text}");
            }
            let headline = format!("{when} · {symbol} · {result}");
            let mut codes = vec![format!("環境 {mode_text}"), format!("狀態碼 {state}")];
            if let Some(raw) = &block_reason {
                codes.push(format!("原始訊息 {raw}"));
            }
            AttemptView { uuid: pair.to_string(), symbol, at: ts, simulated, mode_label, mode_text, legs, state, state_text, failed_checks, block_reason, needs_manual, when, headline, codes: codes.join(" · "), net_pnl }
        })
        .collect()
}

#[cfg(test)]
#[path = "staged_orders_tests.rs"]
mod tests;
