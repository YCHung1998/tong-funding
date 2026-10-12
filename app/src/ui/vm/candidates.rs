//! Scanner "加入交易單" column and Candidate List (ui-trading-pages spec scanner-candidates).
//! Pure. Ticking a row only changes this in-memory list (lost on restart, by design); "加入並前往
//! 交易單" sends one `AddPrepared` per still-valid candidate and never an order.

use std::collections::BTreeSet;

use serde_json::json;
use tong_funding_core::pair::PairState;
use tong_funding_core::types::{Decimal, Exchange};

use super::bridge::{CommandSink, UiSnapshot};
use super::leverage_cap::{self, CapCheck};
use super::scanner::{Qualified, ScanRow, ScannerVm};
use crate::engine::command::{Command, NewPreparedPair, PairView};

/// Why a row cannot be ticked (first failing rule, in this order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateBlock {
    NoDirection,
    /// Settings incomplete: qualification cannot be decided.
    NotDecidable,
    NotQualified,
    /// A leg's market source failed on its latest attempt (old data never becomes a candidate).
    StaleData(Exchange),
    SettlementUnknown,
    SettlementPassed,
    /// The symbol already has a PREPARED or running pair (one that still holds exposure).
    AlreadyStaged,
}

impl CandidateBlock {
    pub fn text(self) -> String {
        match self {
            CandidateBlock::NoDirection => "無可交易方向".into(),
            CandidateBlock::NotDecidable => "設定不完整".into(),
            CandidateBlock::NotQualified => "未達標".into(),
            CandidateBlock::StaleData(e) => format!("{} 行情過期", e.name()),
            CandidateBlock::SettlementUnknown => "結算時間未知".into(),
            CandidateBlock::SettlementPassed => "已過結算時間".into(),
            CandidateBlock::AlreadyStaged => "已在交易單".into(),
        }
    }
}

/// An account read older than this is not evidence of "flat" (the pages poll every 15 s).
const ACCOUNT_FRESH_MS: i64 = 45_000;

/// Both legs' accounts were read completely and recently and show no position and no open order
/// on the pair's symbol. Anything unknown (error, incomplete list, stale, never read) is `false`.
fn legs_flat(snap: &UiSnapshot, p: &PairView, now_ms: i64) -> bool {
    [p.long_exchange, p.short_exchange].into_iter().all(|ex| {
        let Some(acc) = snap.leg_accounts.get(&(p.simulated, ex)) else { return false };
        if now_ms.saturating_sub(acc.fetched_at) > ACCOUNT_FRESH_MS {
            return false;
        }
        let (Ok(positions), Ok(orders)) = (&acc.positions, &acc.open_orders) else { return false };
        positions.complete
            && orders.complete
            && !positions.items.iter().any(|x| x.symbol == p.symbol && !x.quantity.is_zero())
            && !orders.items.iter().any(|x| x.symbol == p.symbol)
    })
}

/// Whether `symbol` still has a pair that holds it. A pair with no exposure left does not (re-adding
/// a closed symbol, candidate-readd-after-close): `CLOSING` after the flat confirmation, and a locked
/// state whose legs are verifiably flat on the accounts. Such a pair itself is left as it is.
fn staged(snap: &UiSnapshot, symbol: &str, now_ms: i64) -> bool {
    snap.engine.as_ref().is_some_and(|e| {
        e.pairs.iter().any(|p| {
            p.symbol == symbol
                && match p.state {
                    PairState::Finalized | PairState::Cancelled | PairState::Blocked => false,
                    PairState::Closing => !p.flat_confirmed,
                    PairState::PartialFailure | PairState::Imbalanced | PairState::Unresolved => !legs_flat(snap, p, now_ms),
                    PairState::Prepared | PairState::PreTradeCheck | PairState::OrderSubmit | PairState::FillMonitor | PairState::Reconciled => true,
                }
        })
    })
}

/// Whether the row may be ticked. Pure.
pub fn eligibility(row: &ScanRow, snap: &UiSnapshot, now_ms: i64) -> Result<(), CandidateBlock> {
    let Some((long, short)) = row.direction else { return Err(CandidateBlock::NoDirection) };
    match row.qualified {
        Qualified::Yes => {}
        Qualified::NotConfigured => return Err(CandidateBlock::NotDecidable),
        Qualified::No | Qualified::NotApplicable => return Err(CandidateBlock::NotQualified),
    }
    for ex in [long, short] {
        if snap.market.get(&ex).is_none_or(|f| f.failing() || f.last_success_at.is_none()) {
            return Err(CandidateBlock::StaleData(ex));
        }
    }
    match row.countdown_target {
        None => return Err(CandidateBlock::SettlementUnknown),
        Some((t, _)) if t <= now_ms => return Err(CandidateBlock::SettlementPassed),
        Some(_) => {}
    }
    if staged(snap, &row.symbol, now_ms) {
        return Err(CandidateBlock::AlreadyStaged);
    }
    Ok(())
}

/// The ticked symbols (memory only).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidateList {
    symbols: BTreeSet<String>,
}

impl CandidateList {
    /// Tick / untick; an ineligible row cannot be ticked (untick is always allowed). Returns
    /// whether the list changed.
    pub fn toggle(&mut self, row: &ScanRow, snap: &UiSnapshot, now_ms: i64) -> bool {
        if self.symbols.remove(&row.symbol) {
            return true;
        }
        if eligibility(row, snap, now_ms).is_ok() {
            self.symbols.insert(row.symbol.clone());
            return true;
        }
        false
    }

    pub fn contains(&self, symbol: &str) -> bool {
        self.symbols.contains(symbol)
    }

    pub fn remove(&mut self, symbol: &str) {
        self.symbols.remove(symbol);
    }

    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateView {
    pub symbol: String,
    pub long: Exchange,
    pub short: Exchange,
    pub gross_spread: Option<Decimal>,
    pub net_edge_pct: Option<Decimal>,
    pub settlement_ms: Option<i64>,
    pub notional: Decimal,
    pub leverage: Decimal,
    pub margin: Decimal,
    pub long_price: Option<Decimal>,
    pub short_price: Option<Decimal>,
    /// symbol-leverage-cap: both legs' caps against the contract leverage (shown; a known cap
    /// below the leverage also makes the candidate invalid, an unknown one does not).
    pub cap: CapCheck,
    /// Re-checked on every data update; `Err` = shown with the reason, never added.
    pub valid: Result<(), String>,
}

fn price(snap: &UiSnapshot, ex: Exchange, symbol: &str) -> Option<Decimal> {
    snap.market.get(&ex)?.observations.iter().find(|o| o.symbol == symbol).map(|o| o.mark_price)
}

/// The Candidate List as shown, re-validated against the latest scan. Pure.
pub fn views(list: &CandidateList, scanner: &ScannerVm, snap: &UiSnapshot, now_ms: i64) -> Vec<CandidateView> {
    let t = snap.settings.contract;
    list.symbols
        .iter()
        .map(|symbol| {
            let row = scanner.rows.iter().find(|r| &r.symbol == symbol);
            let (long, short) = row.and_then(|r| r.direction).unwrap_or((Exchange::Binance, Exchange::Bybit));
            let valid = match (row, &snap.settings.contract_error) {
                (_, Some(e)) => Err(format!("合約模板無效：{e}")),
                (None, None) => Err("已不在掃描結果中".into()),
                (Some(r), None) => eligibility(r, snap, now_ms).map_err(CandidateBlock::text),
            };
            let (long_price, short_price) = (price(snap, long, symbol), price(snap, short, symbol));
            let valid = valid.and_then(|()| if long_price.is_some() && short_price.is_some() { Ok(()) } else { Err("無掃描價格".into()) });
            let cap = leverage_cap::check(snap, long, short, symbol, t.notional_usdt, t.leverage, now_ms);
            // Only a KNOWN cap below the leverage refuses the add; unknown stays informational here
            // (the engine's pre-trade gate fails closed in EXCHANGE_DEMO).
            let valid = valid.and_then(|()| match cap.blocked_reason(Some(tong_funding_core::risk::ExecutionMode::Simulation)) {
                Some(why) => Err(why),
                None => Ok(()),
            });
            CandidateView {
                symbol: symbol.clone(),
                long,
                short,
                gross_spread: row.and_then(|r| r.gross_spread),
                net_edge_pct: row.and_then(|r| r.net_edge.value()),
                settlement_ms: row.and_then(|r| r.countdown_target).map(|(t, _)| t),
                notional: t.notional_usdt,
                leverage: t.leverage,
                margin: t.margin(),
                long_price,
                short_price,
                cap,
                valid,
            }
        })
        .collect()
}

/// "加入並前往交易單": one `AddPrepared` per valid candidate (scan snapshot in `entry`, read by
/// Node 0 as `EntrySnapshot`). Returns how many were sent. Never an order.
pub fn add_to_staged(list: &CandidateList, scanner: &ScannerVm, snap: &UiSnapshot, now_ms: i64, sink: &dyn CommandSink) -> usize {
    add_to_staged_from(list, scanner, snap, now_ms, sink, "manual")
}

/// [`add_to_staged`] with the origin recorded in the pair's entry (`"manual"` = the button,
/// `"auto_add"` = the automatic adding).
pub fn add_to_staged_from(list: &CandidateList, scanner: &ScannerVm, snap: &UiSnapshot, now_ms: i64, sink: &dyn CommandSink, source: &str) -> usize {
    let mut n = 0;
    for (i, v) in views(list, scanner, snap, now_ms).into_iter().enumerate() {
        let (Ok(()), Some(lp), Some(sp), Some(t)) = (&v.valid, v.long_price, v.short_price, v.settlement_ms) else { continue };
        let uuid = format!("ui-{now_ms}-{i}-{}", v.symbol.to_ascii_lowercase());
        let entry = json!({
            "long_scan_price": lp.normalize().to_string(),
            "short_scan_price": sp.normalize().to_string(),
            "notional_usdt": v.notional.normalize().to_string(),
            "leverage": v.leverage.normalize().to_string(),
            "gross_spread": v.gross_spread.map(|g| g.normalize().to_string()),
            "net_edge_pct": v.net_edge_pct.map(|g| g.normalize().to_string()),
            "scanned_at_ms": now_ms,
            "source": source,
        });
        sink.send(
            format!("{} {}", if source == "auto_add" { "自動加入交易單" } else { "加入交易單" }, v.symbol),
            Command::AddPrepared(NewPreparedPair {
                internal_uuid: uuid,
                pair_id: format!("{}-{now_ms}", v.symbol),
                symbol: v.symbol.clone(),
                long_exchange: v.long,
                short_exchange: v.short,
                settlement_ms: t,
                entry,
            }),
        );
        n += 1;
    }
    n
}

// ---- automatic adding --------------------------------------------------------------------------------

/// A row is only added automatically with at least this long before its entry trigger (the entry
/// lead is 10 s): a pair added too close to the settlement would only miss its window.
pub const AUTO_ADD_MIN_LEAD_MS: i64 = 90_000;

/// The automatic adding of qualified scanner rows to the staged orders. Off at every start (never
/// stored: after a restart nothing trades by itself until the user turns it on again).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutoAddState {
    pub enabled: bool,
    /// The first click on "開啟" asked for confirmation.
    pub confirming: bool,
    /// (symbol, settlement time) already auto-added in this run: a pair that was cancelled or
    /// blocked is not added again for the same settlement (no add → cancel → add loop).
    attempted: BTreeSet<(String, i64)>,
    pub added_total: usize,
    pub last_added_ms: Option<i64>,
    pub last_symbols: Vec<String>,
}

impl AutoAddState {
    /// First click: ask for confirmation (nothing is turned on yet).
    pub fn request_enable(&mut self) {
        if !self.enabled {
            self.confirming = true;
        }
    }
    /// Second click: turn it on.
    pub fn confirm_enable(&mut self) {
        if self.confirming {
            self.enabled = true;
            self.confirming = false;
        }
    }
    pub fn cancel_confirm(&mut self) {
        self.confirming = false;
    }
    pub fn disable(&mut self) {
        self.enabled = false;
        self.confirming = false;
    }
}

/// Why nothing is added automatically right now, if so (shown next to the button).
pub fn auto_add_blocked(snap: &UiSnapshot) -> Option<String> {
    let Some(engine) = &snap.engine else { return Some("引擎未啟動".into()) };
    if let Some(e) = &snap.settings.error {
        return Some(format!("設定不完整（{e}）"));
    }
    if let Some(e) = &snap.settings.contract_error {
        return Some(format!("合約模板無效（{e}）"));
    }
    let refusing = super::engine_view::refusing_blockers(engine);
    refusing.first().map(super::engine_view::blocker_text)
}

/// Pairs that occupy a slot of `max_concurrent_pairs` (the same rule as the engine's Node 0).
fn active_pairs(snap: &UiSnapshot) -> usize {
    snap.engine.as_ref().map_or(0, |e| {
        e.pairs
            .iter()
            .filter(|p| !matches!(p.state, PairState::Finalized | PairState::Cancelled | PairState::Blocked) && !(p.state == PairState::Closing && p.flat_confirmed))
            .count()
    })
}

/// The symbols to add now, best Net Edge first, at most as many as free pair slots remain. A row
/// qualifies when it could be ticked by hand AND its candidate is valid (known leverage caps),
/// has at least [`AUTO_ADD_MIN_LEAD_MS`] left, was not auto-added for this settlement before and
/// has no pair of any state for this settlement.
pub fn auto_add_symbols(scanner: &ScannerVm, snap: &UiSnapshot, state: &AutoAddState, now_ms: i64) -> Vec<String> {
    if !state.enabled || auto_add_blocked(snap).is_some() {
        return Vec::new();
    }
    let room = (snap.settings.risk.max_concurrent_pairs as usize).saturating_sub(active_pairs(snap));
    if room == 0 {
        return Vec::new();
    }
    let engine_pairs = snap.engine.as_ref().map(|e| e.pairs.as_slice()).unwrap_or(&[]);
    let mut picks: Vec<(String, Option<Decimal>)> = Vec::new();
    for row in &scanner.rows {
        let Some((t, _)) = row.countdown_target else { continue };
        if eligibility(row, snap, now_ms).is_err()
            || t - now_ms < AUTO_ADD_MIN_LEAD_MS
            || state.attempted.contains(&(row.symbol.clone(), t))
            || engine_pairs.iter().any(|p| p.symbol == row.symbol && p.settlement_ms == t)
        {
            continue;
        }
        picks.push((row.symbol.clone(), row.net_edge.value()));
    }
    // Best Net Edge first (a row without a value last).
    picks.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut list = CandidateList::default();
    list.symbols = picks.iter().map(|p| p.0.clone()).collect();
    let valid: BTreeSet<String> = views(&list, scanner, snap, now_ms).into_iter().filter(|v| v.valid.is_ok()).map(|v| v.symbol).collect();
    picks.into_iter().map(|p| p.0).filter(|s| valid.contains(s)).take(room).collect()
}

/// One pass of the automatic adding: sends one `AddPrepared` per symbol of [`auto_add_symbols`] and
/// remembers them. Never an order (a PREPARED pair is only a plan; the entry still goes through
/// the engine's trigger mode and its pre-trade check). Returns how many were sent.
pub fn auto_add_tick(state: &mut AutoAddState, scanner: &ScannerVm, snap: &UiSnapshot, now_ms: i64, sink: &dyn CommandSink) -> usize {
    let symbols = auto_add_symbols(scanner, snap, state, now_ms);
    if symbols.is_empty() {
        return 0;
    }
    let mut list = CandidateList::default();
    list.symbols = symbols.iter().cloned().collect();
    let n = add_to_staged_from(&list, scanner, snap, now_ms, sink, "auto_add");
    for s in &symbols {
        if let Some(t) = scanner.rows.iter().find(|r| &r.symbol == s).and_then(|r| r.countdown_target).map(|(t, _)| t) {
            state.attempted.insert((s.clone(), t));
        }
    }
    if n > 0 {
        state.added_total += n;
        state.last_added_ms = Some(now_ms);
        state.last_symbols = symbols;
    }
    n
}

#[cfg(test)]
#[path = "candidates_tests.rs"]
mod tests;
