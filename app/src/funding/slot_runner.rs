//! Creates and checks funding slots (change funding-slot-tracing, tasks 3.1–3.4). One
//! [`slots_tick`] at `now_ms`: create the slots of pairs that were holding through a settlement,
//! then check every due slot — queries merged per (exchange, symbol), matched to the ledger rows
//! the exchange returned, settled or moved on by the pure [`step`] machine. Every decision is made
//! from the injected `now_ms` and the store, so a restart or a machine that slept simply finds the
//! overdue slots due and checks them at once (catch-up).

use std::collections::{BTreeMap, HashSet};

use tong_funding_core::pnl::SLOT_MATCH_TOLERANCE_MS;
use tong_funding_core::types::{Decimal, Exchange};

use super::fetch::{BoxFut, FetchStatus, LedgerSource, Pause, fetch_and_store};
use super::pnl_record::{assemble, recompute_if_changed};
use super::runner::{FetchKey, Readiness};
use super::slots::{CheckOutcome, RateCheck, STAGE2_END_MS, Step, first_check_ms, step};
use crate::store::db::Db;
use crate::store::funding_ledger::StoredLedgerEntry;
use crate::store::funding_slots::{ManualReason, SlotLeg, SlotRow, SlotState};

/// The exchange's public funding-rate history: the rate that applied at a settlement time.
/// `Ok(None)` = the history has no rate for that time; `Err` = the lookup failed.
pub trait RateSource: Send + Sync {
    fn rate_at<'a>(&'a self, exchange: Exchange, symbol: &'a str, settle_ms: i64) -> BoxFut<'a, Result<Option<Decimal>, String>>;
}

/// No rate lookups (the rate stays unknown, so nothing is ever settled as zero).
pub struct NoRates;
impl RateSource for NoRates {
    fn rate_at<'a>(&'a self, _: Exchange, _: &'a str, _: i64) -> BoxFut<'a, Result<Option<Decimal>, String>> {
        Box::pin(std::future::ready(Ok(None)))
    }
}

/// What one slot tick did.
#[derive(Debug, Default, PartialEq)]
pub struct SlotTickReport {
    pub created: usize,
    /// (pair, leg, settlement time, outcome label) per check made.
    pub checked: Vec<(String, SlotLeg, i64, &'static str)>,
    pub settled: Vec<(String, SlotLeg, i64)>,
    pub errors: Vec<String>,
}

impl SlotTickReport {
    pub fn wrote(&self) -> bool {
        self.created > 0 || !self.checked.is_empty()
    }
}

/// Slots the store should have for every non-simulated pair: one per (leg, settlement time `t`)
/// with `opened < t ≤ closed` (or `≤ now` while the pair is still open), derived exactly like the
/// PnL's expected settlements. A slot whose first check lies more than the whole tracing period in
/// the past is created as `MANUAL_CHECK / BACKFILL`, due now (an earlier incomplete pair).
/// Idempotent. A finalized pair whose closing fill is unknown gets no slot: its holding period is
/// not known, so nothing is claimed.
pub fn create_slots(db: &Db, now_ms: i64) -> Result<usize, String> {
    if db.is_halted() {
        return Ok(0);
    }
    let mut created = 0;
    for row in db.list_pairs().map_err(|e| e.to_string())? {
        let Ok(a) = assemble(db, &row.internal_uuid, now_ms) else { continue };
        if a.simulated {
            continue;
        }
        let finalized = row.status == "FINALIZED";
        for (i, leg) in [SlotLeg::Long, SlotLeg::Short].into_iter().enumerate() {
            let Some(w) = &a.windows[i] else { continue };
            if finalized && w.closed_at_ms.is_none() {
                continue;
            }
            for &t in &a.slots[i] {
                if t > now_ms {
                    continue;
                }
                let mut slot = SlotRow::new(&row.internal_uuid, leg, w.exchange, &w.symbol, t, first_check_ms(t), now_ms);
                if now_ms.saturating_sub(t) >= STAGE2_END_MS {
                    slot.state = SlotState::ManualCheck;
                    slot.manual_reason = Some(ManualReason::Backfill);
                    slot.next_check_ms = now_ms;
                }
                if db.insert_slot_if_absent(&slot).map_err(|e| e.to_string())? {
                    created += 1;
                }
            }
        }
    }
    Ok(created)
}

fn outcome_label(o: &CheckOutcome) -> &'static str {
    match o {
        CheckOutcome::Found { .. } => "found",
        CheckOutcome::NotFound { rate: RateCheck::Zero { .. } } => "zero_rate",
        CheckOutcome::NotFound { .. } => "not_found",
        CheckOutcome::Failed { .. } => "failed",
        CheckOutcome::Ambiguous { .. } => "ambiguous",
    }
}

/// The ledger rows an exchange returned for `slot` (same exchange and symbol, stamped within the
/// settlement tolerance, not linked to another slot).
fn candidates<'a>(entries: &'a [StoredLedgerEntry], slot: &SlotRow, linked: &HashSet<String>) -> Vec<&'a StoredLedgerEntry> {
    entries
        .iter()
        .filter(|e| {
            e.entry.exchange == slot.exchange
                && e.entry.symbol == slot.symbol
                && (e.entry.settled_at_ms - slot.settle_ms).abs() <= SLOT_MATCH_TOLERANCE_MS
                && !linked.contains(&e.entry.dedupe_key)
        })
        .collect()
}

/// Checks every due slot once. A group with no source, no keys or an incomplete fetch counts as a
/// failed check for each of its slots (never as "nothing found").
pub async fn check_due_slots(db: &Db, sources: &[&dyn LedgerSource], rates: &dyn RateSource, ready: Readiness<'_>, pause: &dyn Pause, now_ms: i64, report: &mut SlotTickReport) {
    if db.is_halted() {
        return;
    }
    let due = match db.due_slots(now_ms) {
        Ok(d) => d,
        Err(e) => {
            report.errors.push(e.to_string());
            return;
        }
    };
    if due.is_empty() {
        return;
    }
    let source_of = |ex: Exchange| sources.iter().copied().find(|s| s.exchange() == ex);
    let key_of = |s: &SlotRow| FetchKey { exchange: s.exchange, symbol: source_of(s.exchange).is_none_or(|src| src.per_symbol()).then(|| s.symbol.clone()) };
    let mut groups: BTreeMap<FetchKey, Vec<SlotRow>> = BTreeMap::new();
    for s in due {
        groups.entry(key_of(&s)).or_default().push(s);
    }
    // Slots of other pairs sharing (exchange, symbol, time): a single ledger row cannot be split.
    let all = db.all_slots().unwrap_or_default();
    for (key, slots) in groups {
        // One query per group over the union of its slots' windows.
        let failure: Option<String> = match source_of(key.exchange) {
            None => Some(format!("no ledger source for {}", key.exchange.name())),
            Some(source) => match ready(key.exchange) {
                Err(why) => Some(why),
                Ok(()) => {
                    let start = slots.iter().map(|s| s.settle_ms).min().unwrap_or(now_ms) - SLOT_MATCH_TOLERANCE_MS;
                    let symbol = key.symbol.clone().unwrap_or_else(|| slots[0].symbol.clone());
                    match fetch_and_store(db, source, pause, &symbol, start, now_ms, now_ms).await {
                        Ok(f) if f.status == FetchStatus::Complete => None,
                        Ok(f) => Some(match f.status {
                            FetchStatus::Incomplete(r) => r,
                            _ => "超出交易所保留範圍".to_string(),
                        }),
                        Err(e) => Some(e),
                    }
                }
            },
        };
        let entries = if failure.is_none() { db.funding_ledger_entries().unwrap_or_default() } else { Vec::new() };
        let linked: HashSet<String> = all.iter().filter_map(|s| s.ledger_dedupe_key.clone()).collect();
        let mut taken: HashSet<String> = HashSet::new();
        for slot in slots {
            let outcome = match &failure {
                Some(why) => CheckOutcome::Failed { error: why.clone() },
                None => {
                    let rivals = all.iter().filter(|o| o.pair_id != slot.pair_id && o.exchange == slot.exchange && o.symbol == slot.symbol && o.settle_ms == slot.settle_ms).count();
                    let mut found: Vec<&StoredLedgerEntry> = candidates(&entries, &slot, &linked).into_iter().filter(|e| !taken.contains(&e.entry.dedupe_key)).collect();
                    found.sort_by_key(|e| e.entry.settled_at_ms);
                    match found.first() {
                        Some(_) if rivals > 0 => CheckOutcome::Ambiguous { detail: format!("{rivals} other pair(s) held {} on {} at that settlement", slot.symbol, slot.exchange.name()) },
                        Some(e) => {
                            taken.insert(e.entry.dedupe_key.clone());
                            let first_seen = db.event_ts_ms(e.event_id).ok().flatten().unwrap_or(now_ms);
                            CheckOutcome::Found { amount: e.entry.amount, dedupe_key: e.entry.dedupe_key.clone(), exchange_id: e.entry.exchange_id.clone(), first_seen_ms: first_seen }
                        }
                        None => {
                            let rate = match rates.rate_at(slot.exchange, &slot.symbol, slot.settle_ms).await {
                                Ok(Some(r)) if r.is_zero() => RateCheck::Zero { source: format!("{} public funding-rate history", slot.exchange.name()) },
                                Ok(Some(_)) => RateCheck::NonZero,
                                Ok(None) | Err(_) => RateCheck::Unknown,
                            };
                            CheckOutcome::NotFound { rate }
                        }
                    }
                }
            };
            report.checked.push((slot.pair_id.clone(), slot.leg, slot.settle_ms, outcome_label(&outcome)));
            match step(&slot, now_ms, outcome) {
                Step::Progress(next) => {
                    if let Err(e) = db.update_slot_progress(&next) {
                        report.errors.push(e.to_string());
                    }
                }
                Step::Settled(done) => match db.settle_slot(&done) {
                    Ok(true) => {
                        report.settled.push((slot.pair_id.clone(), slot.leg, slot.settle_ms));
                        // The PnL of a pair that already has one follows its new funding at once.
                        if let Err(e) = recompute_if_changed(db, &slot.pair_id, now_ms) {
                            report.errors.push(e);
                        }
                    }
                    Ok(false) => report.errors.push(format!("slot {}/{}/{} not settled: already settled or the ledger row is linked elsewhere", slot.pair_id, slot.leg.as_str(), slot.settle_ms)),
                    Err(e) => report.errors.push(e.to_string()),
                },
            }
        }
    }
}

/// Per exchange: how many settled-by-ledger slots, and the p50/p95/p99 of `first_seen − t` (ms).
/// Calibrates the 5/10/30/60 minute constants (design D9).
pub fn delay_report(db: &Db) -> Result<BTreeMap<Exchange, (usize, (i64, i64, i64))>, String> {
    let mut by: BTreeMap<Exchange, Vec<i64>> = BTreeMap::new();
    for s in db.all_slots().map_err(|e| e.to_string())? {
        if s.state == SlotState::Settled
            && s.resolution == Some(crate::store::funding_slots::Resolution::Ledger)
            && let Some(seen) = s.first_seen_ms
        {
            by.entry(s.exchange).or_default().push(seen - s.settle_ms);
        }
    }
    Ok(by.into_iter().filter_map(|(ex, d)| super::slots::delay_percentiles(&d).map(|p| (ex, (d.len(), p)))).collect())
}

/// One pass: create the slots, then check the due ones.
pub async fn slots_tick(db: &Db, sources: &[&dyn LedgerSource], rates: &dyn RateSource, ready: Readiness<'_>, pause: &dyn Pause, now_ms: i64) -> SlotTickReport {
    let mut report = SlotTickReport::default();
    match create_slots(db, now_ms) {
        Ok(n) => report.created = n,
        Err(e) => report.errors.push(e),
    }
    check_due_slots(db, sources, rates, ready, pause, now_ms, &mut report).await;
    report
}

#[cfg(test)]
#[path = "slot_runner_tests.rs"]
mod tests;
