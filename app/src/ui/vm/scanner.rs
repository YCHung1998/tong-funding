//! Scanner view-model (task 2.4, spec scanner-page). Net Edge, qualification and the best pair
//! always come from `core` (design D3); this module only selects rows, picks what to show and
//! formats it. Every enabled exchange (`allowed_exchanges`), OKX included, takes part in Gross
//! Spread, Net Edge, direction and qualification (okx-trading-enablement).

use std::collections::{BTreeMap, BTreeSet};

use tong_funding_core::edge_kind::{EdgeClassification, EdgeKind, classify_edge};
use tong_funding_core::funding::{DataStatus, FundingObservation, equivalent_8h_rate, is_consistent_listed};
use tong_funding_core::net_edge::{NetEdgeParams, best_opportunity_with, legs_settle_together, meets_min_expected_net_pnl};
use tong_funding_core::risk::{EffectiveConfig, RiskConfig, RiskOverrides, effective_for_pair};
use tong_funding_core::types::{Decimal, Exchange};

use super::alerts::FreshStatus;
use super::banner;
use super::bridge::{ClockState, MarketFeed, Settings, SourceId, UiSnapshot, tradable_exchanges};
use super::labels::disabled_source_text;
use super::format::{self, DASH};
use crate::ui::theme::{Tone, funding_tone};

/// Net Edge in percent does not depend on the notional (every term is proportional to it), so any
/// positive value gives the same percentage (design D3).
pub const SCAN_NOTIONAL_USDT: i64 = 1_000;

/// Marker on a rate whose source is failing or stale.
pub const STALE_NOTE: &str = "過期";
/// Gross Spread label when the two legs settle on different intervals.
pub const GROSS_INTERVALS_DIFFER: &str = "週期不同";

/// The opportunity-kind tag (`週期錯位`, `費率差`).
pub fn edge_kind_text(k: EdgeKind) -> &'static str {
    match k {
        EdgeKind::IntervalMismatch => "週期錯位",
        EdgeKind::RateGap => "費率差",
    }
}

/// One exchange's cell in a row.
#[derive(Debug, Clone, PartialEq)]
pub enum RateCell {
    /// Exchange not enabled, no data, or `NOT_LISTED`.
    Empty,
    /// `DATA_ERROR`: never used in any calculation.
    DataError { interval_unknown: bool },
    Rate {
        rate: Decimal,
        interval_secs: i64,
        /// 8h equivalent, display only, present when the interval is not 8h.
        eq_8h: Option<Decimal>,
        /// The source's latest fetch failed (or it is stale): this value is old.
        stale: bool,
    },
}

impl RateCell {
    pub fn text(&self) -> String {
        match self {
            RateCell::Empty => DASH.into(),
            RateCell::DataError { .. } => "資料異常".into(),
            RateCell::Rate { rate, .. } => format::rate_pct(*rate),
        }
    }
    /// Line 1 of the cell: the rate (or `—` / `資料異常`), the interval tag, the stale marker.
    pub fn first_line(&self) -> Vec<String> {
        let mut parts = vec![self.text()];
        parts.extend(self.tag());
        if let RateCell::Rate { stale: true, .. } = self {
            parts.push(STALE_NOTE.into());
        }
        parts
    }
    /// Line 2 of the cell: the 8h equivalent (display only), present when the interval is not 8h.
    pub fn second_line(&self) -> Option<String> {
        match self {
            RateCell::Rate { eq_8h: Some(e), .. } => Some(format!("8h 等效 {}（僅供顯示）", format::rate_pct(*e))),
            _ => None,
        }
    }
    /// Interval tag (`4h`, `8h`, `週期未知`), if any.
    pub fn tag(&self) -> Option<String> {
        match self {
            RateCell::Empty => None,
            RateCell::DataError { interval_unknown } => interval_unknown.then(|| "週期未知".into()),
            RateCell::Rate { interval_secs, .. } => Some(format::interval_label(Some(*interval_secs))),
        }
    }
    pub fn tone(&self) -> Tone {
        match self {
            RateCell::Rate { rate, .. } => funding_tone(*rate),
            _ => Tone::Muted,
        }
    }
}

/// Net Edge column.
#[derive(Debug, Clone, PartialEq)]
pub enum NetEdgeCell {
    /// Fewer than two tradable `LISTED` legs.
    NotApplicable,
    /// Required settings are missing; lists them (e.g. `Bybit taker_fee_pct`).
    NotConfigured(Vec<String>),
    Value(Decimal),
}

impl NetEdgeCell {
    pub fn text(&self) -> String {
        match self {
            NetEdgeCell::NotApplicable => DASH.into(),
            NetEdgeCell::NotConfigured(missing) => format!("未設定（缺 {}）", missing.join("、")),
            NetEdgeCell::Value(v) => format::fixed(*v, 4),
        }
    }
    pub fn value(&self) -> Option<Decimal> {
        match self {
            NetEdgeCell::Value(v) => Some(*v),
            _ => None,
        }
    }
    pub fn tone(&self) -> Tone {
        match self {
            NetEdgeCell::Value(v) if v.is_sign_negative() && !v.is_zero() => Tone::Negative,
            NetEdgeCell::Value(_) => Tone::Positive,
            _ => Tone::Muted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qualified {
    Yes,
    No,
    NotConfigured,
    NotApplicable,
    /// The legs settle at different times (or one time is missing): never 達標, whatever the Net Edge
    /// (require-synchronized-settlement).
    NotSynchronized,
}

impl Qualified {
    pub fn text(self) -> &'static str {
        match self {
            Qualified::Yes => "達標",
            Qualified::No | Qualified::NotApplicable => DASH,
            Qualified::NotSynchronized => "結算不同步",
            Qualified::NotConfigured => "未設定",
        }
    }
}

/// Countdown state of a row (computed at paint time, see [`countdown`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Countdown {
    Remaining { ms: i64, calibrated: bool },
    /// Reached or past the settlement: shown as "結算中 · 待更新", never negative.
    Settling,
    Unknown,
}

impl Countdown {
    /// Line 1 when the cell is drawn on two lines: the time, or `結算中`.
    pub fn first_line(self) -> String {
        match self {
            Countdown::Remaining { ms, .. } => format::hms(ms),
            Countdown::Settling => "結算中".into(),
            Countdown::Unknown => DASH.into(),
        }
    }
    /// Line 2: `（未校時）` for an uncalibrated clock, `待更新` while settling.
    pub fn second_line(self) -> Option<String> {
        match self {
            Countdown::Remaining { calibrated: false, .. } => Some("（未校時）".into()),
            Countdown::Settling => Some("待更新".into()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScanRow {
    pub rank: usize,
    pub symbol: String,
    pub listed: usize,
    pub enabled: usize,
    /// Binance, Bybit, OKX (fixed column order).
    pub cells: [(Exchange, RateCell); 3],
    /// `(long, short)` of the best pair; `None` when not applicable.
    pub direction: Option<(Exchange, Exchange)>,
    pub gross_spread: Option<Decimal>,
    pub intervals_differ: bool,
    pub net_edge: NetEdgeCell,
    pub qualified: Qualified,
    /// Core's classification of the shown pair (display only): where the income comes from and the
    /// hourly rate spread. `None` = no pair, or core could not classify it (unknown interval, overflow).
    pub edge: Option<EdgeClassification>,
    /// Settlement target and the exchange whose clock it is on.
    pub countdown_target: Option<(i64, Exchange)>,
}

impl ScanRow {
    pub fn coverage_text(&self) -> String {
        format!("{}/{}", self.listed, self.enabled)
    }
    pub fn direction_text(&self) -> String {
        match self.direction {
            Some((l, s)) => format!("L {} / S {}", l.name(), s.name()),
            None => DASH.into(),
        }
    }
    /// The direction on two lines (`L Bybit`, `S Binance`) for the narrow column.
    pub fn direction_lines(&self) -> Option<(String, String)> {
        self.direction.map(|(l, s)| (format!("L {}", l.name()), format!("S {}", s.name())))
    }
    pub fn gross_text(&self) -> String {
        self.gross_spread.map_or_else(|| DASH.into(), format::rate_pct)
    }
    /// Opportunity-kind tag (`週期錯位`, `費率差`); none when there is no classification or no income.
    pub fn edge_kind_label(&self) -> Option<&'static str> {
        self.edge.as_ref()?.kind.map(edge_kind_text)
    }
    /// Labels under the Gross Spread value, in this order: `週期不同` (the legs settle on different
    /// intervals). The opportunity-kind tag is drawn after it (it carries a tooltip).
    pub fn gross_tags(&self) -> Vec<&'static str> {
        if self.intervals_differ { vec![GROSS_INTERVALS_DIFFER] } else { Vec::new() }
    }
    /// Tooltip of the tag: what the kind means plus the hourly rate spread.
    pub fn edge_kind_hint(&self) -> Option<String> {
        let edge = self.edge.as_ref()?;
        let meaning = match edge.kind? {
            EdgeKind::IntervalMismatch => "收入多半來自兩所結算週期不同，而非每小時費率差",
            EdgeKind::RateGap => "收入主要來自每小時費率差",
        };
        Some(format!("{meaning}\n每小時費率差 {} %/h（僅供顯示）", format::fixed(edge.hourly_spread_pct_per_hour, 4)))
    }
}

/// Page-level state of the table.
#[derive(Debug, Clone, PartialEq)]
pub enum TableState {
    /// No market data has arrived yet.
    Loading,
    /// Every enabled source failed and nothing was ever loaded, or all failed on the latest attempt.
    Unavailable { errors: Vec<(Exchange, String)> },
    Ready,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScannerSummary {
    pub scanned: usize,
    pub multi_coverage: usize,
    /// `None` = qualification cannot be decided (settings incomplete).
    pub qualified: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScannerVm {
    pub rows: Vec<ScanRow>,
    pub summary: ScannerSummary,
    pub state: TableState,
    /// Errors of sources whose latest attempt failed (shown even when the table is usable).
    pub source_errors: Vec<(Exchange, String)>,
    /// Header: the required-spread formula per enabled exchange pair (read-only; edited on the risk page).
    pub breakdown: Vec<BreakdownLine>,
    /// Qualification can be decided (toggle enabled).
    pub decidable: bool,
    /// When this table was computed (local ms): "最新掃描".
    pub computed_at: i64,
    /// Age of the oldest successful fetch among the enabled sources (old data carries its age).
    pub oldest_data_age_ms: Option<i64>,
}

/// What the table shows for the toggle.
#[derive(Debug, Clone, PartialEq)]
pub enum RowsView<'a> {
    Rows(Vec<&'a ScanRow>),
    /// Toggle on, nothing qualifies.
    NoneQualified,
}

impl ScannerVm {
    /// "符合 N 筆" (same number with the toggle on or off), or the reason it cannot be decided.
    pub fn match_text(&self) -> String {
        match self.summary.qualified {
            Some(n) if self.decidable => format!("符合 {n} 筆"),
            _ => "設定不完整，無法判斷達標".into(),
        }
    }

    /// Rows for the "只顯示達標" toggle (ignored while not decidable).
    pub fn visible(&self, only_qualified: bool) -> RowsView<'_> {
        if !only_qualified || !self.decidable {
            return RowsView::Rows(self.rows.iter().collect());
        }
        let rows: Vec<_> = self.rows.iter().filter(|r| r.qualified == Qualified::Yes).collect();
        if rows.is_empty() { RowsView::NoneQualified } else { RowsView::Rows(rows) }
    }
}

/// `allowed_coins` holds coins (`BTC`); core's qualification compares symbols, so both the row
/// filter and core get the same `<COIN>USDT` set (entries already ending in USDT are kept).
fn allowed_symbols(coins: &[String]) -> BTreeSet<String> {
    coins
        .iter()
        .map(|c| {
            let c = c.trim().to_ascii_uppercase();
            if c.ends_with("USDT") { c } else { format!("{c}USDT") }
        })
        .collect()
}

/// The core parameters for a pair of exchanges, from the conservatively merged settings.
fn params_for(risk: &RiskConfig, overrides: &RiskOverrides, enabled: &BTreeSet<Exchange>, symbols: &BTreeSet<String>, x: Exchange, y: Exchange) -> NetEdgeParams {
    let eff = effective_for_pair(risk, overrides, x, y);
    let mut taker_fee_pct = BTreeMap::new();
    if let Some(f) = eff.long_taker_fee_pct {
        taker_fee_pct.insert(x, f);
    }
    if let Some(f) = eff.short_taker_fee_pct {
        taker_fee_pct.insert(y, f);
    }
    NetEdgeParams {
        taker_fee_pct,
        est_slippage_pct: eff.est_slippage_pct,
        safety_margin_pct: eff.safety_margin_pct,
        net_edge_threshold_pct: eff.net_edge_threshold_pct,
        min_24h_volume_usdt: eff.min_24h_volume_usdt,
        allowed_exchanges: enabled.clone(),
        allowed_coins: symbols.clone(),
    }
}

/// Required Net Edge settings missing for the pair `(x, y)`, named for the user.
fn missing_for(settings: &Settings, x: Exchange, y: Exchange) -> Vec<String> {
    if settings.error.is_some() {
        return vec!["風控設定（讀取失敗）".into()];
    }
    let eff: EffectiveConfig = effective_for_pair(&settings.risk, &settings.overrides, x, y);
    let mut out = Vec::new();
    if eff.net_edge_threshold_pct.is_none() {
        out.push("net_edge_threshold_pct".to_string());
    }
    if eff.est_slippage_pct.is_none() {
        out.push("est_slippage_pct".to_string());
    }
    if eff.long_taker_fee_pct.is_none() {
        out.push(format!("{} taker_fee_pct", x.name()));
    }
    if eff.short_taker_fee_pct.is_none() {
        out.push(format!("{} taker_fee_pct", y.name()));
    }
    out
}

/// One exchange pair's "required funding spread" worked out from its merged settings
/// (spec scanner-threshold-breakdown). The same Net Edge formula as core, moved to the spread side:
/// `spread >= threshold + 2(fee_L + fee_S) + 4 slippage + safety_margin`.
#[derive(Debug, Clone, PartialEq)]
pub struct BreakdownLine {
    pub pair: (Exchange, Exchange),
    /// `Err` = the settings that are missing (never replaced by 0).
    pub calc: Result<Breakdown, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Breakdown {
    pub threshold: Decimal,
    pub fee_long: Decimal,
    pub fee_short: Decimal,
    pub slippage: Decimal,
    pub safety_margin: Decimal,
    pub min_expected_net_pnl: Decimal,
}

impl Breakdown {
    fn costs(&self) -> Decimal {
        Decimal::from(2) * (self.fee_long + self.fee_short) + Decimal::from(4) * self.slippage
    }
    pub fn required(&self) -> Decimal {
        self.threshold + self.costs() + self.safety_margin
    }
    /// The `min_expected_net_pnl_pct` condition (Net Edge before the safety margin).
    pub fn min_pnl_required(&self) -> Decimal {
        self.min_expected_net_pnl + self.costs()
    }
}

impl BreakdownLine {
    /// Display lines: the formula, plus a second formula when the minimum-net-PnL condition is stricter.
    pub fn lines(&self) -> Vec<String> {
        let name = format!("{}↔{}", self.pair.0.name(), self.pair.1.name());
        let f = |v: Decimal| format::fixed(v, 4);
        match &self.calc {
            Err(missing) => vec![format!("{name}  無法計算：缺少 {}", missing.join("、"))],
            Ok(b) => {
                let fees = format!("2×({}+{})", f(b.fee_long), f(b.fee_short));
                let mut out = vec![format!(
                    "{name}  所需費率價差 % = {} + {fees} + 4×{} + {} = {}",
                    f(b.threshold),
                    f(b.slippage),
                    f(b.safety_margin),
                    f(b.required())
                )];
                if b.min_pnl_required() > b.required() {
                    out.push(format!(
                        "最低淨利 = {} + {fees} + 4×{} = {}　以較嚴者為準：{}",
                        f(b.min_expected_net_pnl),
                        f(b.slippage),
                        f(b.min_pnl_required()),
                        f(b.min_pnl_required())
                    ));
                }
                out
            }
        }
    }
}

/// One line per unordered pair of `enabled` exchanges, from the same merged settings core uses.
pub fn threshold_breakdown(settings: &Settings, enabled: &[Exchange]) -> Vec<BreakdownLine> {
    pairs_of(enabled)
        .into_iter()
        .map(|(x, y)| {
            let missing = missing_for(settings, x, y);
            let eff = effective_for_pair(&settings.risk, &settings.overrides, x, y);
            let calc = match (missing.is_empty(), eff.net_edge_threshold_pct, eff.est_slippage_pct, eff.long_taker_fee_pct, eff.short_taker_fee_pct) {
                (true, Some(threshold), Some(slippage), Some(fee_long), Some(fee_short)) => Ok(Breakdown {
                    threshold,
                    fee_long,
                    fee_short,
                    slippage,
                    safety_margin: eff.safety_margin_pct,
                    min_expected_net_pnl: eff.min_expected_net_pnl_pct,
                }),
                _ => Err(missing),
            };
            BreakdownLine { pair: (x, y), calc }
        })
        .collect()
}

fn pairs_of(exchanges: &[Exchange]) -> Vec<(Exchange, Exchange)> {
    let mut out = Vec::new();
    for (i, a) in exchanges.iter().enumerate() {
        for b in &exchanges[i + 1..] {
            out.push((*a, *b));
        }
    }
    out
}

fn cell_for(o: Option<&FundingObservation>, enabled: bool, feed_stale: bool) -> RateCell {
    let Some(o) = o.filter(|_| enabled) else { return RateCell::Empty };
    if is_consistent_listed(o) {
        let interval = o.funding_interval_secs.unwrap_or_default();
        return RateCell::Rate {
            rate: o.funding_rate,
            interval_secs: interval,
            eq_8h: if interval != 28_800 { equivalent_8h_rate(o.funding_rate, o.funding_interval_secs) } else { None },
            stale: feed_stale,
        };
    }
    match (o.data_status, o.funding_interval_secs) {
        (DataStatus::NotListed, _) => RateCell::Empty,
        // Too old to use: shown, marked, never used in a calculation.
        (DataStatus::Stale, Some(interval)) => RateCell::Rate { rate: o.funding_rate, interval_secs: interval, eq_8h: None, stale: true },
        (_, interval) => RateCell::DataError { interval_unknown: interval.is_none() },
    }
}

/// Earliest settlement among `legs`, with the exchange whose clock it is on.
fn earliest(legs: &[&FundingObservation]) -> Option<(i64, Exchange)> {
    legs.iter().filter(|o| o.next_funding_time > 0).map(|o| (o.next_funding_time, o.exchange)).min_by_key(|(t, _)| *t)
}

/// What a row shows for its pairing columns.
struct Pairing<'a> {
    legs: Option<(&'a FundingObservation, &'a FundingObservation)>,
    net: NetEdgeCell,
    qualified: Qualified,
}

/// Pairing columns for one symbol: core decides when settings are complete; otherwise the pair
/// with the largest gross spread is shown (long = lower rate) with the missing settings.
fn pairing<'a>(candidates: &[&'a FundingObservation], settings: &Settings, enabled: &BTreeSet<Exchange>, symbols: &BTreeSet<String>) -> Pairing<'a> {
    let mut gross_best: Option<(&FundingObservation, &FundingObservation, Decimal)> = None;
    let mut missing: Vec<String> = Vec::new();
    for (i, a) in candidates.iter().enumerate() {
        for b in &candidates[i + 1..] {
            let g = (a.funding_rate - b.funding_rate).abs();
            if gross_best.is_none_or(|(_, _, best)| g > best) {
                let (l, s) = if a.funding_rate <= b.funding_rate { (*a, *b) } else { (*b, *a) };
                gross_best = Some((l, s, g));
            }
            for m in missing_for(settings, a.exchange, b.exchange) {
                if !missing.contains(&m) {
                    missing.push(m);
                }
            }
        }
    }
    let fallback = |missing: Vec<String>| Pairing { legs: gross_best.map(|(l, s, _)| (l, s)), net: NetEdgeCell::NotConfigured(missing), qualified: Qualified::NotConfigured };
    if !missing.is_empty() {
        return fallback(missing);
    }
    let owned: Vec<FundingObservation> = candidates.iter().map(|o| (*o).clone()).collect();
    let pf = |x: Exchange, y: Exchange| params_for(&settings.risk, &settings.overrides, enabled, symbols, x, y);
    match best_opportunity_with(&owned, Decimal::from(SCAN_NOTIONAL_USDT), &pf) {
        Ok(Some(opp)) => {
            let pick = |ex: Exchange| candidates.iter().copied().find(|o| o.exchange == ex);
            Pairing {
                legs: pick(opp.edge.long).zip(pick(opp.edge.short)),
                net: NetEdgeCell::Value(opp.edge.net_edge_pct),
                // ui-trading-pages: "達標" also needs expected net PnL ≥ min_expected_net_pnl_pct
                // (same rule as Node 0's NetEdgeQualified).
                qualified: if let Some((l, s)) = pick(opp.edge.long).zip(pick(opp.edge.short))
                    && !legs_settle_together(l, s)
                {
                    // require-synchronized-settlement: shown whatever the Net Edge is.
                    Qualified::NotSynchronized
                } else if opp.qualifies
                    && meets_min_expected_net_pnl(&opp.edge, Decimal::from(SCAN_NOTIONAL_USDT), settings.risk.min_expected_net_pnl_pct)
                {
                    Qualified::Yes
                } else {
                    Qualified::No
                },
            }
        }
        Ok(None) => Pairing { legs: None, net: NetEdgeCell::NotApplicable, qualified: Qualified::NotApplicable },
        Err(e) => fallback(vec![e.to_string()]),
    }
}

/// Builds the scanner table from a snapshot. Pure.
pub fn build(snap: &UiSnapshot, now_ms: i64) -> ScannerVm {
    let settings = &snap.settings;
    let risk = &settings.risk;
    let enabled: BTreeSet<Exchange> = risk.allowed_exchanges.iter().copied().collect();
    let symbols_allowed = allowed_symbols(&risk.allowed_coins);
    let empty = MarketFeed::default();
    let feed = |e: Exchange| snap.market.get(&e).unwrap_or(&empty);

    // symbol -> exchange -> observation (enabled exchanges only)
    let mut table: BTreeMap<&str, BTreeMap<Exchange, &FundingObservation>> = BTreeMap::new();
    for e in &enabled {
        for o in &feed(*e).observations {
            table.entry(o.symbol.as_str()).or_default().insert(*e, o);
        }
    }

    let tradable: Vec<Exchange> = enabled.iter().copied().filter(|e| tradable_exchanges(snap.identity).contains(e)).collect();
    let tradable_pairs = pairs_of(&tradable);
    let decidable = settings.error.is_none()
        && if tradable_pairs.is_empty() {
            risk.net_edge_threshold_pct.is_some()
        } else {
            tradable_pairs.iter().all(|(x, y)| missing_for(settings, *x, *y).is_empty())
        };

    let mut rows = Vec::new();
    for (symbol, by_ex) in &table {
        let listed: Vec<&FundingObservation> = by_ex.values().copied().filter(|o| is_consistent_listed(o)).collect();
        if listed.is_empty() || (!symbols_allowed.is_empty() && !symbols_allowed.contains(*symbol)) {
            continue;
        }
        let cells = Exchange::ALL.map(|e| (e, cell_for(by_ex.get(&e).copied(), enabled.contains(&e), feed(e).failing())));
        let candidates: Vec<&FundingObservation> = listed.iter().copied().filter(|o| tradable_exchanges(snap.identity).contains(&o.exchange)).collect();
        let mut row = ScanRow {
            rank: 0,
            symbol: symbol.to_string(),
            listed: listed.len(),
            enabled: enabled.len(),
            cells,
            direction: None,
            gross_spread: None,
            intervals_differ: false,
            net_edge: NetEdgeCell::NotApplicable,
            qualified: Qualified::NotApplicable,
            edge: None,
            countdown_target: earliest(&listed),
        };
        if candidates.len() >= 2 {
            let p = pairing(&candidates, settings, &enabled, &symbols_allowed);
            if let Some((l, s)) = p.legs {
                row.direction = Some((l.exchange, s.exchange));
                row.gross_spread = Some((s.funding_rate - l.funding_rate).abs());
                row.intervals_differ = l.funding_interval_secs != s.funding_interval_secs;
                row.countdown_target = earliest(&[l, s]);
                row.edge = classify_edge(l, s);
            }
            row.net_edge = p.net;
            row.qualified = p.qualified;
        }
        rows.push(row);
    }

    let gross_desc = |a: &ScanRow, b: &ScanRow| b.gross_spread.cmp(&a.gross_spread).then_with(|| a.symbol.cmp(&b.symbol));
    if decidable {
        rows.sort_by(|a, b| match (a.net_edge.value(), b.net_edge.value()) {
            (Some(x), Some(y)) => y.cmp(&x).then_with(|| gross_desc(a, b)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => gross_desc(a, b),
        });
    } else {
        rows.sort_by(gross_desc);
    }
    for (i, r) in rows.iter_mut().enumerate() {
        r.rank = i + 1;
    }

    let summary = ScannerSummary {
        scanned: rows.len(),
        multi_coverage: rows.iter().filter(|r| r.listed >= 2).count(),
        qualified: decidable.then(|| rows.iter().filter(|r| r.qualified == Qualified::Yes).count()),
    };

    let feeds: Vec<(Exchange, &MarketFeed)> = enabled.iter().map(|e| (*e, feed(*e))).collect();
    let source_errors: Vec<(Exchange, String)> =
        feeds.iter().filter(|(_, f)| f.failing()).filter_map(|(e, f)| f.last_error.as_ref().map(|(m, _)| (*e, m.clone()))).collect();
    let state = if !feeds.is_empty() && feeds.iter().all(|(_, f)| f.failing()) {
        TableState::Unavailable { errors: source_errors.clone() }
    } else if feeds.iter().all(|(_, f)| f.last_success_at.is_none()) {
        TableState::Loading
    } else {
        TableState::Ready
    };
    let oldest_data_age_ms = feeds.iter().filter_map(|(_, f)| f.last_success_at).min().map(|t| (now_ms - t).max(0));

    let breakdown = threshold_breakdown(settings, &tradable);

    ScannerVm { rows, summary, state, source_errors, breakdown, decidable, computed_at: now_ms, oldest_data_age_ms }
}

/// The countdown of a row: `target − (now + offset of the target's exchange)`. An unsynced clock
/// falls back to local time and is labelled.
pub fn countdown(target: Option<(i64, Exchange)>, clock: ClockState, now_ms: i64) -> Countdown {
    let Some((t, _)) = target else { return Countdown::Unknown };
    let (offset, calibrated) = match clock {
        ClockState::Synced { offset_ms } => (offset_ms, true),
        ClockState::Unsynced => (0, false),
    };
    let remaining = t - now_ms.saturating_add(offset);
    if remaining <= 0 { Countdown::Settling } else { Countdown::Remaining { ms: remaining, calibrated } }
}

/// The sources shown in the scanner header: the Binance and Bybit WebSockets, Bybit and OKX polls.
pub const HEADER_SOURCES: [SourceId; 4] = [SourceId::BinanceWs, SourceId::BybitWs, SourceId::MarketPoll(Exchange::Bybit), SourceId::MarketPoll(Exchange::Okx)];

/// One source in the scanner header (scanner-layout): its name and its state, each said once.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceChip {
    /// The exchange is not in `allowed_exchanges`: one muted line, no status, never a red OFFLINE.
    Disabled { exchange: Exchange },
    /// The Bybit WebSocket was switched off by the roll-back switch.
    RolledBack { label: String },
    Live { label: String, status: FreshStatus, age_s: Option<i64>, next_poll_s: Option<i64>, websocket: bool },
}

impl SourceChip {
    /// Element id and ordering key.
    pub fn key(&self) -> String {
        match self {
            SourceChip::Disabled { exchange } => format!("off-{}", exchange.name()),
            SourceChip::RolledBack { label } | SourceChip::Live { label, .. } => label.clone(),
        }
    }
    /// The source's name; for a disabled exchange the whole line (`OKX 已停用`).
    pub fn label(&self) -> String {
        match self {
            SourceChip::Disabled { exchange } => disabled_source_text(*exchange),
            SourceChip::RolledBack { label } | SourceChip::Live { label, .. } => label.clone(),
        }
    }
    /// The state after the name: `ONLINE · 3 秒前 · 下次 7s`. The WebSockets use ONLINE / RECONNECTING /
    /// OFFLINE, the polls the shared freshness words. Empty for a disabled exchange.
    pub fn detail(&self) -> String {
        match self {
            SourceChip::Disabled { .. } => String::new(),
            SourceChip::RolledBack { .. } => BYBIT_WS_DISABLED_TEXT.into(),
            SourceChip::Live { status, age_s, next_poll_s, websocket, .. } => {
                let word = if *websocket {
                    match status {
                        FreshStatus::Online => "ONLINE",
                        FreshStatus::Stale | FreshStatus::RateLimited => "RECONNECTING",
                        FreshStatus::Offline | FreshStatus::Unknown => "OFFLINE",
                    }
                } else {
                    status.label()
                };
                let mut parts = vec![word.to_string()];
                parts.extend(age_s.map(|a| format!("{a} 秒前")));
                parts.extend(next_poll_s.map(|n| format!("下次 {n}s")));
                parts.join(" · ")
            }
        }
    }
    /// Status dot, if the source has a status.
    pub fn dot(&self) -> Option<FreshStatus> {
        match self {
            SourceChip::Live { status, .. } => Some(*status),
            _ => None,
        }
    }
}

/// The header's source chips. A source of an exchange that is not ticked becomes one `已停用` line
/// (a single one per exchange, even when it has both a WebSocket and a poll).
pub fn source_chips(snap: &UiSnapshot, now_ms: i64) -> Vec<SourceChip> {
    let mut out: Vec<SourceChip> = Vec::new();
    for source in HEADER_SOURCES {
        let ex = source.exchange();
        let chip = if !snap.exchange_enabled(ex) {
            SourceChip::Disabled { exchange: ex }
        } else if source == SourceId::BybitWs && snap.bybit_ws_disabled {
            SourceChip::RolledBack { label: source.label() }
        } else {
            let f = banner::freshness(snap, source, now_ms);
            let websocket = matches!(source, SourceId::BinanceWs | SourceId::BybitWs);
            SourceChip::Live { label: source.label(), status: f.status, age_s: f.age_s, next_poll_s: if websocket { None } else { f.next_poll_s }, websocket }
        };
        if !out.contains(&chip) {
            out.push(chip);
        }
    }
    out
}

/// Text of the Bybit WebSocket indicator when the roll-back switch turned it off.
pub const BYBIT_WS_DISABLED_TEXT: &str = "已停用（TONG_FUNDING_BYBIT_FEED=rest）";

#[cfg(test)]
#[path = "scanner_tests.rs"]
mod tests;
