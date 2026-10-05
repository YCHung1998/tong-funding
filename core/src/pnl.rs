//! Pair PnL accounting (change funding-pnl, spec pnl-accounting). Pure functions only: the caller
//! hands in fills, attributed funding ledger entries and the expected settlement count; nothing
//! here reads a clock, the store or the network. Money is `Decimal`; nothing is rounded for display.
//!
//! Formula (SYSTEM_SPEC §31 as read in design D4):
//! `Net = Funding + Price(reference) − Opening fee − Closing fee − Slippage − Other`, where
//! `Slippage = Price(reference) − Price(actual)`, so `Net = Funding + Price(actual) − fees − Other`
//! too and no price difference is deducted twice.
//!
//! Missing data is never a 0: a component that cannot be fully computed is listed in `missing`
//! (its value is the partial sum of what is known) and the status is `INCOMPLETE` with reasons.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{Decimal, Exchange, Side};

/// The only fee / funding asset that is counted without a conversion.
pub const QUOTE_ASSET: &str = "USDT";

// ---- funding ledger -------------------------------------------------------------------------

/// One funding payment as the exchange reported it, normalised (spec funding-history-fetch):
/// `amount` is positive when received and negative when paid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FundingLedgerEntry {
    pub exchange: Exchange,
    pub symbol: String,
    pub amount: Decimal,
    pub asset: String,
    /// Settlement time (Unix ms) as the exchange stamped the entry.
    pub settled_at_ms: i64,
    /// The exchange's id of the entry (Binance `tranId`, Bybit `id`).
    pub exchange_id: String,
    /// The exchange's entry type (Binance `incomeType`, Bybit `type`).
    pub kind: String,
    /// `binance:{incomeType}:{tranId}` / `bybit:{id}` (design D2).
    pub dedupe_key: String,
    /// The whole row as received.
    pub raw: Value,
}

impl FundingLedgerEntry {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        exchange: Exchange,
        symbol: impl Into<String>,
        amount: Decimal,
        asset: impl Into<String>,
        settled_at_ms: i64,
        exchange_id: impl Into<String>,
        kind: impl Into<String>,
        raw: Value,
    ) -> FundingLedgerEntry {
        let exchange_id = exchange_id.into();
        let kind = kind.into();
        let dedupe_key = dedupe_key(exchange, &kind, &exchange_id);
        FundingLedgerEntry { exchange, symbol: symbol.into(), amount, asset: asset.into(), settled_at_ms, exchange_id, kind, dedupe_key, raw }
    }
}

/// Design D2: Binance ids are unique per `incomeType`, so the type is part of the key; Bybit ids
/// are unique on their own.
pub fn dedupe_key(exchange: Exchange, kind: &str, exchange_id: &str) -> String {
    match exchange {
        Exchange::Binance => format!("binance:{kind}:{exchange_id}"),
        Exchange::Bybit => format!("bybit:{exchange_id}"),
        Exchange::Okx => format!("okx:{kind}:{exchange_id}"),
    }
}

// ---- inputs ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FillAction {
    Open,
    Close,
}

/// Direction of one trade; slippage is adverse when a buy fills higher or a sell fills lower.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TradeDirection {
    Buy,
    Sell,
}

impl TradeDirection {
    /// Opening a long and closing a short buy; the other two sell.
    pub const fn of(side: Side, action: FillAction) -> TradeDirection {
        match (side, action) {
            (Side::Long, FillAction::Open) | (Side::Short, FillAction::Close) => TradeDirection::Buy,
            (Side::Long, FillAction::Close) | (Side::Short, FillAction::Open) => TradeDirection::Sell,
        }
    }
}

/// One order's (cumulative) fill as recorded by the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FillRecord {
    /// `client_order_id`.
    pub id: String,
    pub action: FillAction,
    /// Filled quantity in base coin.
    pub quantity: Decimal,
    /// Reference price recorded when the order was sent; `None` = not recorded.
    pub expected_price: Option<Decimal>,
    /// Average fill price; `None` = not reported.
    pub actual_price: Option<Decimal>,
    /// Fee paid (positive = cost, negative = rebate), in `fee_asset`; `None` = not reported.
    pub fee: Option<Decimal>,
    pub fee_asset: Option<String>,
    /// When the fill was recorded (Unix ms).
    pub filled_at_ms: i64,
}

/// Whether the funding ledger behind a leg could be fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FundingFetchState {
    Fetched,
    /// Not fetched at all yet (e.g. keys unavailable).
    NotFetched,
    Failed(String),
    /// The window starts before what the exchange keeps.
    BeyondRetention,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegInput {
    pub exchange: Exchange,
    pub symbol: String,
    pub side: Side,
    pub fills: Vec<FillRecord>,
    /// Ledger entries already attributed to this leg ([`attribute`]).
    pub funding: Vec<FundingLedgerEntry>,
    /// Settlements expected while the leg was held ([`expected_settlements`]); `None` = cannot be derived.
    pub expected_settlements: Option<usize>,
    pub funding_fetch: FundingFetchState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairInput {
    pub legs: Vec<LegInput>,
    /// Some ledger entry in the pair's windows could belong to another pair as well.
    pub ambiguous_attribution: bool,
    /// The latest reconciliation of the pair found a difference.
    pub reconciliation_mismatch: bool,
}

// ---- outputs --------------------------------------------------------------------------------

/// The named parts of a PnL result (for "missing" marks and display).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Component {
    Funding,
    PriceRef,
    PriceActual,
    OpeningFee,
    ClosingFee,
    Slippage,
    OtherCost,
    Net,
}

/// Values in USDT. Fees and slippage are costs (positive = paid / adverse).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Components {
    pub funding: Decimal,
    pub price_ref: Decimal,
    pub price_actual: Decimal,
    pub opening_fee: Decimal,
    pub closing_fee: Decimal,
    pub slippage: Decimal,
    /// Always 0 in this version (no data source); see `PnlBreakdown::other_cost_included`.
    pub other_cost: Decimal,
    pub net: Decimal,
}

impl Components {
    fn add(&mut self, o: &Components) {
        self.funding += o.funding;
        self.price_ref += o.price_ref;
        self.price_actual += o.price_actual;
        self.opening_fee += o.opening_fee;
        self.closing_fee += o.closing_fee;
        self.slippage += o.slippage;
        self.other_cost += o.other_cost;
        self.net += o.net;
    }

    fn normalized(mut self) -> Components {
        for v in [
            &mut self.funding,
            &mut self.price_ref,
            &mut self.price_actual,
            &mut self.opening_fee,
            &mut self.closing_fee,
            &mut self.slippage,
            &mut self.other_cost,
            &mut self.net,
        ] {
            *v = v.normalize();
        }
        self
    }

    pub fn get(&self, c: Component) -> Decimal {
        match c {
            Component::Funding => self.funding,
            Component::PriceRef => self.price_ref,
            Component::PriceActual => self.price_actual,
            Component::OpeningFee => self.opening_fee,
            Component::ClosingFee => self.closing_fee,
            Component::Slippage => self.slippage,
            Component::OtherCost => self.other_cost,
            Component::Net => self.net,
        }
    }
}

/// Why a result is INCOMPLETE (spec "PnL 狀態與不完整判定").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IncompleteReason {
    MissingSettlement { exchange: Exchange, expected: usize, received: usize },
    SettlementsNotDerivable { exchange: Exchange },
    FundingNotFetched { exchange: Exchange },
    FundingFetchFailed { exchange: Exchange, reason: String },
    BeyondRetention { exchange: Exchange },
    MissingFillDetail { exchange: Exchange, id: String, what: String },
    FeeNotConvertible { exchange: Exchange, id: String, asset: String },
    MissingReferencePrice { exchange: Exchange, id: String },
    OpenCloseQuantityMismatch { exchange: Exchange, opened: Decimal, closed: Decimal },
    AmbiguousAttribution,
    ReconciliationMismatch,
    /// No leg has any recorded fill: a PnL is only asked for pairs that held exposure, so the fill
    /// details are missing (never a complete 0).
    NoFillsRecorded,
}

impl IncompleteReason {
    /// Text shown in the UI (named reason, never a silent 0).
    pub fn label(&self) -> String {
        match self {
            IncompleteReason::MissingSettlement { exchange, expected, received } => {
                format!("缺少結算流水（{}：預期 {expected} 筆，已取得 {received} 筆）", exchange.name())
            }
            IncompleteReason::SettlementsNotDerivable { exchange } => format!("無法推算預期結算次數（{}）", exchange.name()),
            IncompleteReason::FundingNotFetched { exchange } => format!("funding 流水尚未取得（{}）", exchange.name()),
            IncompleteReason::FundingFetchFailed { exchange, reason } => format!("funding 流水取得失敗（{}：{reason}）", exchange.name()),
            IncompleteReason::BeyondRetention { exchange } => format!("超出交易所保留範圍（{}）", exchange.name()),
            IncompleteReason::MissingFillDetail { exchange, id, what } => format!("成交明細缺漏（{} {id}：{what}）", exchange.name()),
            IncompleteReason::FeeNotConvertible { exchange, id, asset } => format!("手續費無法換算（{} {id}：{asset}）", exchange.name()),
            IncompleteReason::MissingReferencePrice { exchange, id } => format!("無參考價（{} {id}）", exchange.name()),
            IncompleteReason::OpenCloseQuantityMismatch { exchange, opened, closed } => {
                format!("開平倉數量不一致（{}：開 {opened}，平 {closed}）", exchange.name())
            }
            IncompleteReason::AmbiguousAttribution => "流水歸屬不明".to_string(),
            IncompleteReason::ReconciliationMismatch => "對帳差異".to_string(),
            IncompleteReason::NoFillsRecorded => "成交明細缺漏（沒有任何成交紀錄）".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PnlStatus {
    Complete,
    Incomplete(Vec<IncompleteReason>),
}

impl PnlStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PnlStatus::Complete => "COMPLETE",
            PnlStatus::Incomplete(_) => "INCOMPLETE",
        }
    }
    pub fn reasons(&self) -> &[IncompleteReason] {
        match self {
            PnlStatus::Complete => &[],
            PnlStatus::Incomplete(r) => r,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegPnl {
    pub exchange: Exchange,
    pub symbol: String,
    pub side: Side,
    pub components: Components,
    pub missing: BTreeSet<Component>,
    pub settlements_received: usize,
    pub expected_settlements: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PnlBreakdown {
    pub legs: Vec<LegPnl>,
    pub total: Components,
    /// Union of the legs' missing components.
    pub missing: BTreeSet<Component>,
    pub status: PnlStatus,
    /// `false`: "other cost" has no data source in this version (shown as 0, "未納入").
    pub other_cost_included: bool,
}

// ---- computation ----------------------------------------------------------------------------

fn leg_pnl(leg: &LegInput, reasons: &mut Vec<IncompleteReason>) -> LegPnl {
    let ex = leg.exchange;
    let mut c = Components::default();
    let mut missing: BTreeSet<Component> = BTreeSet::new();

    // Funding.
    c.funding = leg.funding.iter().map(|e| e.amount).sum();
    let received = leg.funding.len();
    if let Some(e) = leg.funding.iter().find(|e| e.asset != QUOTE_ASSET) {
        reasons.push(IncompleteReason::FeeNotConvertible { exchange: ex, id: e.dedupe_key.clone(), asset: e.asset.clone() });
        missing.insert(Component::Funding);
    }
    match &leg.funding_fetch {
        FundingFetchState::Fetched => {}
        FundingFetchState::NotFetched => {
            reasons.push(IncompleteReason::FundingNotFetched { exchange: ex });
            missing.insert(Component::Funding);
        }
        FundingFetchState::Failed(reason) => {
            reasons.push(IncompleteReason::FundingFetchFailed { exchange: ex, reason: reason.clone() });
            missing.insert(Component::Funding);
        }
        FundingFetchState::BeyondRetention => {
            reasons.push(IncompleteReason::BeyondRetention { exchange: ex });
            missing.insert(Component::Funding);
        }
    }
    match leg.expected_settlements {
        None => {
            reasons.push(IncompleteReason::SettlementsNotDerivable { exchange: ex });
            missing.insert(Component::Funding);
        }
        Some(expected) if received < expected => {
            reasons.push(IncompleteReason::MissingSettlement { exchange: ex, expected, received });
            missing.insert(Component::Funding);
        }
        Some(_) => {}
    }

    // Fills: fees, reference and actual price PnL.
    let sign = match leg.side {
        Side::Long => Decimal::ONE,
        Side::Short => Decimal::NEGATIVE_ONE,
    };
    let (mut opened, mut closed) = (Decimal::ZERO, Decimal::ZERO);
    for f in leg.fills.iter().filter(|f| !f.quantity.is_zero()) {
        // Price PnL of a long = Σ close (p × q) − Σ open (p × q); a short is the opposite.
        let dir = match f.action {
            FillAction::Open => {
                opened += f.quantity;
                -sign
            }
            FillAction::Close => {
                closed += f.quantity;
                sign
            }
        };
        let fee_component = match f.action {
            FillAction::Open => Component::OpeningFee,
            FillAction::Close => Component::ClosingFee,
        };
        match (f.fee, f.fee_asset.as_deref()) {
            (Some(fee), Some(QUOTE_ASSET)) => match f.action {
                FillAction::Open => c.opening_fee += fee,
                FillAction::Close => c.closing_fee += fee,
            },
            (Some(_), Some(asset)) => {
                reasons.push(IncompleteReason::FeeNotConvertible { exchange: ex, id: f.id.clone(), asset: asset.to_string() });
                missing.insert(fee_component);
            }
            (None, _) | (Some(_), None) => {
                reasons.push(IncompleteReason::MissingFillDetail { exchange: ex, id: f.id.clone(), what: "fee / fee asset".into() });
                missing.insert(fee_component);
            }
        }
        match f.actual_price {
            Some(p) => c.price_actual += dir * p * f.quantity,
            None => {
                reasons.push(IncompleteReason::MissingFillDetail { exchange: ex, id: f.id.clone(), what: "average fill price".into() });
                missing.insert(Component::PriceActual);
                missing.insert(Component::Slippage);
            }
        }
        match f.expected_price {
            Some(p) => c.price_ref += dir * p * f.quantity,
            None => {
                reasons.push(IncompleteReason::MissingReferencePrice { exchange: ex, id: f.id.clone() });
                missing.insert(Component::PriceRef);
                missing.insert(Component::Slippage);
            }
        }
    }
    if opened != closed {
        reasons.push(IncompleteReason::OpenCloseQuantityMismatch { exchange: ex, opened, closed });
        missing.extend([Component::PriceRef, Component::PriceActual, Component::Slippage]);
    }
    c.slippage = c.price_ref - c.price_actual;
    c.other_cost = Decimal::ZERO;
    c.net = c.funding + c.price_ref - c.opening_fee - c.closing_fee - c.slippage - c.other_cost;
    if !missing.is_empty() {
        missing.insert(Component::Net);
    }
    LegPnl {
        exchange: ex,
        symbol: leg.symbol.clone(),
        side: leg.side,
        components: c.normalized(),
        missing,
        settlements_received: received,
        expected_settlements: leg.expected_settlements,
    }
}

/// The breakdown of a pair: per leg and in total, with status and reasons.
pub fn compute_pnl(input: &PairInput) -> PnlBreakdown {
    let mut reasons = Vec::new();
    let legs: Vec<LegPnl> = input.legs.iter().map(|l| leg_pnl(l, &mut reasons)).collect();
    let mut total = Components::default();
    let mut missing = BTreeSet::new();
    for l in &legs {
        total.add(&l.components);
        missing.extend(l.missing.iter().copied());
    }
    if !input.legs.iter().any(|l| l.fills.iter().any(|f| !f.quantity.is_zero())) {
        reasons.push(IncompleteReason::NoFillsRecorded);
        missing.extend([Component::PriceRef, Component::PriceActual, Component::OpeningFee, Component::ClosingFee, Component::Slippage, Component::Net]);
    }
    if input.ambiguous_attribution {
        reasons.push(IncompleteReason::AmbiguousAttribution);
        missing.extend([Component::Funding, Component::Net]);
    }
    if input.reconciliation_mismatch {
        reasons.push(IncompleteReason::ReconciliationMismatch);
        missing.extend([Component::Funding, Component::Net]);
    }
    let status = if reasons.is_empty() { PnlStatus::Complete } else { PnlStatus::Incomplete(reasons) };
    PnlBreakdown { legs, total: total.normalized(), missing, status, other_cost_included: false }
}

// ---- fill ratio and slippage -----------------------------------------------------------------

/// `actual ÷ requested`; `None` when nothing was requested.
pub fn fill_ratio(requested_notional: Decimal, actual_filled_notional: Decimal) -> Option<Decimal> {
    (requested_notional > Decimal::ZERO).then(|| (actual_filled_notional / requested_notional).normalize())
}

/// Slippage % of one fill (a percentage number: 0.01 = 0.01%); positive = adverse.
/// `None` when the expected price is not positive.
pub fn slippage_pct(direction: TradeDirection, expected: Decimal, actual: Decimal) -> Option<Decimal> {
    if expected <= Decimal::ZERO {
        return None;
    }
    let diff = match direction {
        TradeDirection::Buy => actual - expected,
        TradeDirection::Sell => expected - actual,
    };
    Some((diff / expected * Decimal::ONE_HUNDRED).normalize())
}

/// Slippage cost (USDT) of the long legs, the short legs and in total; `None` where unknown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlippageSummary {
    pub long: Option<Decimal>,
    pub short: Option<Decimal>,
    pub total: Option<Decimal>,
}

pub fn slippage_summary(b: &PnlBreakdown) -> SlippageSummary {
    let side = |s: Side| -> Option<Decimal> {
        let mut sum = Decimal::ZERO;
        for l in b.legs.iter().filter(|l| l.side == s) {
            if l.missing.contains(&Component::Slippage) {
                return None;
            }
            sum += l.components.slippage;
        }
        Some(sum.normalize())
    };
    let (long, short) = (side(Side::Long), side(Side::Short));
    let total = long.zip(short).map(|(a, b)| (a + b).normalize());
    SlippageSummary { long, short, total }
}

// ---- attribution and expected settlements ----------------------------------------------------

/// When a leg held its position: from its last opening fill to its last closing fill (`None` =
/// still open; then "now" is the end).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegWindow {
    pub exchange: Exchange,
    pub symbol: String,
    pub opened_at_ms: i64,
    pub closed_at_ms: Option<i64>,
}

impl LegWindow {
    /// `opened < t ≤ closed (or now)`, same exchange and symbol.
    pub fn holds(&self, exchange: Exchange, symbol: &str, t: i64, now_ms: i64) -> bool {
        self.exchange == exchange && self.symbol == symbol && t > self.opened_at_ms && t <= self.closed_at_ms.unwrap_or(now_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Attribution {
    /// Index into the given windows.
    Leg(usize),
    /// No window holds it: kept in the store and shown as "未歸屬".
    Unattributed,
    /// More than one window holds it: never guessed.
    Ambiguous(Vec<usize>),
}

/// Attribution rule "exchange + symbol + time window" (design D3).
pub fn attribute(entry: &FundingLedgerEntry, windows: &[LegWindow], now_ms: i64) -> Attribution {
    let hits: Vec<usize> =
        windows.iter().enumerate().filter(|(_, w)| w.holds(entry.exchange, &entry.symbol, entry.settled_at_ms, now_ms)).map(|(i, _)| i).collect();
    match hits.as_slice() {
        [] => Attribution::Unattributed,
        [one] => Attribution::Leg(*one),
        _ => Attribution::Ambiguous(hits),
    }
}

/// Settlement times `t` with `opened < t ≤ closed` on the grid `first + k × interval`
/// (`first` = the next funding time seen at entry, from funding-observation). Empty when the
/// interval is not positive.
pub fn expected_settlements(first_settlement_ms: i64, interval_secs: i64, opened_at_ms: i64, closed_at_ms: i64) -> Vec<i64> {
    let step = interval_secs.saturating_mul(1000);
    if step <= 0 || closed_at_ms <= opened_at_ms {
        return Vec::new();
    }
    // First grid point strictly after `opened`.
    let mut t = if first_settlement_ms > opened_at_ms {
        let back = (first_settlement_ms - opened_at_ms - 1) / step;
        first_settlement_ms - back * step
    } else {
        let ahead = (opened_at_ms - first_settlement_ms) / step + 1;
        first_settlement_ms + ahead * step
    };
    let mut out = Vec::new();
    while t <= closed_at_ms {
        out.push(t);
        match t.checked_add(step) {
            Some(n) => t = n,
            None => break,
        }
    }
    out
}

// ---- reconciliation ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Reconciliation {
    Ok { count: usize, sum: Decimal },
    Mismatch {
        local_sum: Decimal,
        remote_sum: Decimal,
        /// `remote − local`.
        diff: Decimal,
        local_count: usize,
        remote_count: usize,
        missing_locally: Vec<String>,
        missing_remotely: Vec<String>,
    },
}

/// Exact comparison (design D8: no tolerance) of `(dedupe_key, amount)` lists: the sums, the
/// counts and the key sets must all match.
pub fn reconcile(local: &[(String, Decimal)], remote: &[(String, Decimal)]) -> Reconciliation {
    let local_sum: Decimal = local.iter().map(|(_, a)| *a).sum();
    let remote_sum: Decimal = remote.iter().map(|(_, a)| *a).sum();
    let lk: BTreeMap<&str, Decimal> = local.iter().map(|(k, a)| (k.as_str(), *a)).collect();
    let rk: BTreeMap<&str, Decimal> = remote.iter().map(|(k, a)| (k.as_str(), *a)).collect();
    let missing_locally: Vec<String> = rk.keys().filter(|k| !lk.contains_key(*k)).map(|k| k.to_string()).collect();
    let missing_remotely: Vec<String> = lk.keys().filter(|k| !rk.contains_key(*k)).map(|k| k.to_string()).collect();
    let same_amounts = lk.iter().all(|(k, a)| rk.get(k).is_none_or(|b| a == b));
    if local_sum == remote_sum && local.len() == remote.len() && missing_locally.is_empty() && missing_remotely.is_empty() && same_amounts {
        Reconciliation::Ok { count: local.len(), sum: local_sum.normalize() }
    } else {
        Reconciliation::Mismatch {
            local_sum: local_sum.normalize(),
            remote_sum: remote_sum.normalize(),
            diff: (remote_sum - local_sum).normalize(),
            local_count: local.len(),
            remote_count: remote.len(),
            missing_locally,
            missing_remotely,
        }
    }
}

// ---- expected vs actual -------------------------------------------------------------------------

/// The Net Edge values saved when the pair's orders were sent (engine `entry_snapshot.net_edge`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectedSnapshot {
    pub funding_income: Decimal,
    /// Expected fees of all four fills (a cost).
    pub fee: Decimal,
    /// Expected slippage cost.
    pub slippage: Decimal,
    pub safety_margin: Decimal,
    pub net_edge: Decimal,
    /// Settlements the estimate assumes (net-edge: one).
    pub assumed_settlements: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CompareItem {
    Funding,
    Fee,
    Slippage,
    Net,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompareLine {
    pub item: CompareItem,
    pub expected: Decimal,
    /// `None` when the actual value is incomplete.
    pub actual: Option<Decimal>,
    /// `actual − expected`.
    pub diff: Option<Decimal>,
    /// `diff ÷ |expected| × 100`; `None` when expected is 0 or actual is unknown.
    pub diff_pct: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Comparison {
    /// No snapshot was saved: shown as "無預期快照" (never recomputed afterwards).
    NoSnapshot,
    Lines {
        lines: Vec<CompareLine>,
        /// Expected side only (no actual counterpart).
        safety_margin: Decimal,
        /// `(actual settlements, assumed)` when they differ.
        settlement_note: Option<(usize, u32)>,
    },
}

/// Line-by-line comparison; `actual_settlements` is the number of settlements the pair went
/// through (the caller derives it from the expected settlement times).
pub fn compare_expected(snapshot: Option<&ExpectedSnapshot>, b: &PnlBreakdown, actual_settlements: usize) -> Comparison {
    let Some(s) = snapshot else { return Comparison::NoSnapshot };
    let known = |c: &[Component]| !c.iter().any(|x| b.missing.contains(x));
    let t = &b.total;
    let line = |item: CompareItem, expected: Decimal, actual: Option<Decimal>| {
        let diff = actual.map(|a| (a - expected).normalize());
        let diff_pct = diff.and_then(|d| (!expected.is_zero()).then(|| (d / expected.abs() * Decimal::ONE_HUNDRED).normalize()));
        CompareLine { item, expected, actual: actual.map(|a| a.normalize()), diff, diff_pct }
    };
    let lines = vec![
        line(CompareItem::Funding, s.funding_income, known(&[Component::Funding]).then_some(t.funding)),
        line(CompareItem::Fee, s.fee, known(&[Component::OpeningFee, Component::ClosingFee]).then_some(t.opening_fee + t.closing_fee)),
        line(CompareItem::Slippage, s.slippage, known(&[Component::Slippage]).then_some(t.slippage)),
        line(CompareItem::Net, s.net_edge, known(&[Component::Net]).then_some(t.net)),
    ];
    let settlement_note = (actual_settlements != s.assumed_settlements as usize).then_some((actual_settlements, s.assumed_settlements));
    Comparison::Lines { lines, safety_margin: s.safety_margin, settlement_note }
}

// ---- settlement timeline -------------------------------------------------------------------------

/// State of one expected settlement slot (spec settlement-timeline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlotState {
    /// A ledger entry matches the slot ("已收到").
    Received,
    /// Not yet due, or still within the fetch retry window ("待結算").
    Pending,
    /// Past the retry window without an entry ("缺少").
    Missing,
}

/// How far before its slot time an entry may be stamped and still match it (exchanges may stamp
/// slightly differently; UNVERIFIED, design 實作紀錄).
pub const SLOT_MATCH_TOLERANCE_MS: i64 = 60_000;

/// Matches slot `i` to the first unused entry stamped in `[slot_i − tol, slot_{i+1} − tol)` and
/// returns `(slot time, state, matched entry index)` in slot order. Entries matching no slot are
/// left out (the caller lists them separately).
pub fn match_slots(slots: &[i64], entry_times: &[i64], now_ms: i64, retry_window_ms: i64) -> Vec<(i64, SlotState, Option<usize>)> {
    let mut used = vec![false; entry_times.len()];
    let mut out = Vec::with_capacity(slots.len());
    for (i, &t) in slots.iter().enumerate() {
        let from = t - SLOT_MATCH_TOLERANCE_MS;
        let to = slots.get(i + 1).map_or(i64::MAX, |n| n - SLOT_MATCH_TOLERANCE_MS);
        let hit = entry_times.iter().enumerate().find(|(j, e)| !used[*j] && **e >= from && **e < to).map(|(j, _)| j);
        let state = match hit {
            Some(j) => {
                used[j] = true;
                SlotState::Received
            }
            None if now_ms > t.saturating_add(retry_window_ms) => SlotState::Missing,
            None => SlotState::Pending,
        };
        out.push((t, state, hit));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_match_received_pending_and_missing() {
        let slots = [1_000_000, 2_000_000, 3_000_000];
        let r = match_slots(&slots, &[1_000_500], 2_500_000, 600_000);
        assert_eq!(r[0], (1_000_000, SlotState::Received, Some(0)));
        assert_eq!(r[1].1, SlotState::Pending, "2_000_000 + window not passed yet");
        assert_eq!(r[2].1, SlotState::Pending);
        let r = match_slots(&slots, &[], 3_700_000, 600_000);
        assert_eq!(r.iter().map(|x| x.1).collect::<Vec<_>>(), [SlotState::Missing, SlotState::Missing, SlotState::Missing]);
    }
}
