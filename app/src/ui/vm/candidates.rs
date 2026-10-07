//! Scanner "加入交易單" column and Candidate List (ui-trading-pages spec scanner-candidates).
//! Pure. Ticking a row only changes this in-memory list (lost on restart, by design); "加入並前往
//! 交易單" sends one `AddPrepared` per still-valid candidate and never an order.

use std::collections::BTreeSet;

use serde_json::json;
use tong_funding_core::pair::PairState;
use tong_funding_core::types::{Decimal, Exchange};

use super::bridge::{is_tradable, CommandSink, UiSnapshot};
use super::leverage_cap::{self, CapCheck};
use super::scanner::{Qualified, ScanRow, ScannerVm};
use crate::engine::command::{Command, NewPreparedPair, PairView};

/// Why a row cannot be ticked (first failing rule, in this order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateBlock {
    NoDirection,
    /// The best pair includes OKX (price comparison only).
    CompareOnly,
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
            CandidateBlock::CompareOnly => "OKX 僅比價".into(),
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
    if !is_tradable(long) || !is_tradable(short) {
        return Err(CandidateBlock::CompareOnly);
    }
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
        });
        sink.send(
            format!("加入交易單 {}", v.symbol),
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

#[cfg(test)]
#[path = "candidates_tests.rs"]
mod tests;
