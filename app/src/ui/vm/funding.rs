//! Funding received, settlement timeline, expected-vs-actual panel and data-problem alerts for
//! the positions page (change funding-pnl, spec settlement-timeline, task 4.1). Pure view-model
//! functions plus one store loader (`load_pair_funding`, read-only). Never shows a missing value
//! as 0: "—" plus a named reason.

use std::collections::BTreeMap;

use serde_json::Value;
use tong_funding_core::pnl::{CompareItem, Comparison, Component, FundingFetchState, PnlBreakdown, SlotState, match_slots};
use tong_funding_core::types::{Decimal, Exchange, Side};

use super::format::{self, DASH};
use crate::funding::pnl_record::{assemble, latest_pnl, pair_events};
use crate::funding::{FETCH_ERROR, FUNDING_LEDGER_FETCHED, PNL_RECONCILIATION, PNL_RETRY_WINDOW_MS};
use crate::store::db::Db;
use crate::store::funding_ledger::FUNDING_LEDGER_CONFLICT;
use crate::store::state::PairRow;
use crate::ui::theme::{Tone, funding_tone};

pub const SPREAD_NOTE: &str = "價差，未含 funding 與手續費";
pub const NOT_INCLUDING_CLOSE: &str = "尚未包含平倉成本";
pub const SIMULATED_LABEL: &str = "模擬";
pub const SIMULATED_NO_PNL: &str = "模擬，無實際 PnL";
pub const NO_SNAPSHOT: &str = "無預期快照";
pub const NOT_INCLUDED: &str = "未納入";

/// One leg's funding received.
#[derive(Debug, Clone, PartialEq)]
pub enum LegFunding {
    /// Sum of the entries attributed to the leg (received positive).
    Received(Decimal),
    /// Not shown as a number: why ("尚未取得", "取得失敗：…", "超出交易所保留範圍").
    Unavailable(String),
    /// SIMULATION pair: no ledger.
    Simulated,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineSlot {
    pub time_ms: i64,
    pub side: Side,
    pub exchange: Exchange,
    pub amount: Option<Decimal>,
    pub state: SlotState,
}

/// The newest PnL event of a pair, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct PnlSummary {
    pub event_id: i64,
    pub status: String,
    pub reasons: Vec<String>,
    pub breakdown: Option<PnlBreakdown>,
    pub comparison: Option<Comparison>,
}

/// A data problem shown as an alert, pointing at its event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FundingAlert {
    pub event_id: i64,
    pub event_type: String,
    pub text: String,
}

/// Everything the positions page needs about one pair's funding and PnL.
#[derive(Debug, Clone, PartialEq)]
pub struct PairFunding {
    pub simulated: bool,
    /// (long, short).
    pub legs: [LegFunding; 2],
    /// Opening fees paid so far (USDT); `None` = unknown (missing detail or another asset).
    pub opening_fee: Option<Decimal>,
    /// Time of the newest completed ledger fetch that covers the pair.
    pub updated_at_ms: Option<i64>,
    pub slots: Vec<TimelineSlot>,
    pub pnl: Option<PnlSummary>,
    pub alerts: Vec<FundingAlert>,
}

// ---- loading (read-only) ----------------------------------------------------------------------

fn leg_funding(simulated: bool, held: bool, fetch: &FundingFetchState, amount: Decimal) -> LegFunding {
    if simulated {
        return LegFunding::Simulated;
    }
    if !held {
        return LegFunding::Unavailable("未持倉".into());
    }
    match fetch {
        FundingFetchState::Fetched => LegFunding::Received(amount),
        FundingFetchState::NotFetched => LegFunding::Unavailable("尚未取得".into()),
        FundingFetchState::Failed(r) => LegFunding::Unavailable(format!("取得失敗：{r}")),
        FundingFetchState::BeyondRetention => LegFunding::Unavailable("超出交易所保留範圍".into()),
    }
}

fn decode_pnl(p: &crate::funding::pnl_record::StoredPnl) -> PnlSummary {
    PnlSummary {
        event_id: p.event_id,
        status: p.status().to_string(),
        reasons: serde_json::from_value(p.payload["reasons"].clone()).unwrap_or_default(),
        breakdown: serde_json::from_value(p.payload["breakdown"].clone()).ok(),
        comparison: serde_json::from_value(p.payload["expected_vs_actual"].clone()).ok(),
    }
}

/// Funding data of every given pair row, keyed by `pair_id` (what the UI's `PairInfo` carries).
/// A pair whose data cannot be read is left out (its cells then show "—").
pub fn load_pair_funding(db: &Db, rows: &[PairRow], now_ms: i64) -> BTreeMap<String, PairFunding> {
    let all_events = db.query_events(&crate::store::event_query::EventQuery {
        types: Some([FETCH_ERROR.to_string(), FUNDING_LEDGER_FETCHED.to_string(), FUNDING_LEDGER_CONFLICT.to_string()].into()),
        ..Default::default()
    });
    let global: Vec<(i64, i64, String, Value)> = all_events
        .map(|p| p.rows.into_iter().map(|e| (e.id, e.ts_ms, e.event_type, serde_json::from_str(&e.payload).unwrap_or(Value::Null))).collect())
        .unwrap_or_default();
    let mut out = BTreeMap::new();
    for row in rows {
        let Ok(a) = assemble(db, &row.internal_uuid, now_ms) else { continue };
        let pnl = latest_pnl(db, &row.internal_uuid).ok().flatten().map(|p| decode_pnl(&p));
        let mut legs = [LegFunding::Simulated, LegFunding::Simulated];
        let mut slots = Vec::new();
        let mut updated_at_ms = None;
        for i in 0..2 {
            let leg = &a.input.legs[i];
            let amount: Decimal = leg.funding.iter().map(|e| e.amount).sum();
            legs[i] = leg_funding(a.simulated, a.windows[i].is_some(), &leg.funding_fetch, amount);
            let times: Vec<i64> = leg.funding.iter().map(|e| e.settled_at_ms).collect();
            for (t, state, hit) in match_slots(&a.slots[i], &times, now_ms, PNL_RETRY_WINDOW_MS) {
                slots.push(TimelineSlot { time_ms: t, side: leg.side, exchange: leg.exchange, amount: hit.map(|j| leg.funding[j].amount), state });
            }
        }
        let opening_fee = {
            let fills: Vec<_> = a.input.legs.iter().flat_map(|l| l.fills.iter()).filter(|f| f.action == tong_funding_core::pnl::FillAction::Open && !f.quantity.is_zero()).collect();
            fills.iter().try_fold(Decimal::ZERO, |acc, f| match (f.fee, f.fee_asset.as_deref()) {
                (Some(fee), Some("USDT")) => Some(acc + fee),
                _ => None,
            })
        };
        // Alerts: the latest reconciliation when not OK, an INCOMPLETE latest PnL, ledger
        // conflicts and fetch errors on the pair's exchanges (until a later complete fetch).
        let mut alerts = Vec::new();
        let events = pair_events(db, &row.internal_uuid).unwrap_or_default();
        if let Some(r) = events.iter().filter(|e| e.event_type == PNL_RECONCILIATION).next_back()
            && r.payload["result"].as_str() != Some("OK")
        {
            let result = r.payload["result"].as_str().unwrap_or("FAILED");
            alerts.push(FundingAlert { event_id: r.id, event_type: PNL_RECONCILIATION.into(), text: format!("{} 對帳 {result}", row.symbol) });
        }
        if let Some(p) = &pnl
            && p.status != "COMPLETE"
        {
            alerts.push(FundingAlert { event_id: p.event_id, event_type: "PNL".into(), text: format!("{} PnL INCOMPLETE：{}", row.symbol, p.reasons.join("；")) });
        }
        if !a.simulated {
            let exchanges: Vec<Exchange> = a.input.legs.iter().filter(|l| !l.fills.is_empty()).map(|l| l.exchange).collect();
            let matches = |v: &Value| {
                exchanges.iter().any(|e| v["exchange"].as_str() == Some(e.name())) && v["symbol"].as_str().is_none_or(|s| s == row.symbol)
            };
            for (id, ts, ty, v) in &global {
                if !matches(v) {
                    continue;
                }
                if ty == FUNDING_LEDGER_FETCHED && v["outcome"].as_str() == Some("complete") {
                    updated_at_ms = Some(updated_at_ms.map_or(*ts, |u: i64| u.max(*ts)));
                }
                if ty == FUNDING_LEDGER_CONFLICT {
                    alerts.push(FundingAlert { event_id: *id, event_type: ty.clone(), text: format!("{} funding 流水衝突（{}）", row.symbol, v["dedupe_key"].as_str().unwrap_or("")) });
                }
                if ty == FETCH_ERROR {
                    let resolved = global.iter().any(|(_, t2, ty2, v2)| {
                        ty2 == FUNDING_LEDGER_FETCHED && t2 > ts && v2["outcome"].as_str() == Some("complete") && v2["exchange"] == v["exchange"]
                    });
                    if !resolved {
                        alerts.push(FundingAlert { event_id: *id, event_type: ty.clone(), text: format!("{} funding 流水取得失敗", v["exchange"].as_str().unwrap_or("")) });
                    }
                }
            }
        }
        out.insert(row.pair_id.clone(), PairFunding { simulated: a.simulated, legs, opening_fee, updated_at_ms, slots, pnl, alerts });
    }
    out
}

// ---- view builders ------------------------------------------------------------------------------

/// The "Funding 收到" cell: value and tone (theme's funding colour function), or "—" with a reason.
pub fn leg_cell(f: Option<&LegFunding>) -> (String, Tone) {
    match f {
        Some(LegFunding::Received(v)) => (format!("{} USDT", format::signed(*v, 2)), funding_tone(*v)),
        Some(LegFunding::Unavailable(why)) => (format!("{DASH}（{why}）"), Tone::Muted),
        Some(LegFunding::Simulated) => (SIMULATED_LABEL.to_string(), Tone::Muted),
        None => (format!("{DASH}（尚未取得）"), Tone::Muted),
    }
}

/// Pair total: only when both legs are known numbers.
pub fn pair_total(f: &PairFunding) -> Option<Decimal> {
    match (&f.legs[0], &f.legs[1]) {
        (LegFunding::Received(a), LegFunding::Received(b)) => Some(*a + *b),
        (LegFunding::Received(a), LegFunding::Unavailable(w)) | (LegFunding::Unavailable(w), LegFunding::Received(a)) if w == "未持倉" => Some(*a),
        _ => None,
    }
}

/// `Funding 收到 +0.24 USDT（−0.12 / +0.36）· 更新 08:01:10 UTC`, or "—" with the reason.
pub fn pair_funding_text(f: &PairFunding) -> (String, Tone) {
    if f.simulated {
        return (format!("Funding 收到 {SIMULATED_LABEL}"), Tone::Muted);
    }
    let updated = f.updated_at_ms.map_or_else(String::new, |t| format!(" · 更新 {} UTC", format::utc_hms(t)));
    match pair_total(f) {
        Some(total) => {
            let (l, _) = leg_cell(Some(&f.legs[0]));
            let (s, _) = leg_cell(Some(&f.legs[1]));
            (format!("Funding 收到 {} USDT（{l} / {s}）{updated}", format::signed(total, 2)), funding_tone(total))
        }
        None => {
            let why = f.legs.iter().find_map(|l| match l {
                LegFunding::Unavailable(w) if w != "未持倉" => Some(w.clone()),
                LegFunding::Received(_) | LegFunding::Unavailable(_) | LegFunding::Simulated => None,
            });
            (format!("Funding 收到 {DASH}（{}）{updated}", why.unwrap_or_else(|| "尚未取得".into())), Tone::Muted)
        }
    }
}

/// `進行中合計 X USDT = 價差未實現 + Funding 收到 − 已付開倉手續費（尚未包含平倉成本）`; "—" when a part is unknown.
pub fn running_total_text(f: &PairFunding, spread_unrealized: Option<Decimal>) -> String {
    let fee = f.opening_fee.map_or_else(|| DASH.to_string(), |v| format!("{} USDT", format::money(v, 2)));
    let total = match (spread_unrealized, pair_total(f), f.opening_fee) {
        (Some(s), Some(fu), Some(fee)) if !f.simulated => format!("{} USDT", format::signed(s + fu - fee, 2)),
        _ => DASH.to_string(),
    };
    format!("已付開倉手續費 {fee} · 進行中合計 {total}（{NOT_INCLUDING_CLOSE}）")
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineRow {
    pub time: String,
    pub leg: String,
    pub amount: String,
    pub state: String,
    pub cumulative: String,
    pub tone: Tone,
}

/// Every settlement slot of both legs in time order (ties: long first), with the running total
/// of the received amounts. A missing slot is shown in the warning tone and says the PnL is
/// INCOMPLETE.
pub fn timeline_rows(f: &PairFunding) -> Vec<TimelineRow> {
    let mut slots = f.slots.clone();
    slots.sort_by_key(|s| {
        (
            s.time_ms,
            match s.side {
                Side::Long => 0,
                Side::Short => 1,
            },
        )
    });
    let mut cum = Decimal::ZERO;
    slots
        .iter()
        .map(|s| {
            let side = match s.side {
                Side::Long => "LONG",
                Side::Short => "SHORT",
            };
            let (state, tone) = match s.state {
                SlotState::Received => ("已收到".to_string(), s.amount.map_or(Tone::Muted, funding_tone)),
                SlotState::Pending => ("待結算".to_string(), Tone::Muted),
                SlotState::Missing => ("缺少（PnL 為 INCOMPLETE）".to_string(), Tone::Warning),
            };
            if let Some(a) = s.amount {
                cum += a;
            }
            TimelineRow {
                time: format!("{} UTC", format::utc_hms(s.time_ms)),
                leg: format!("{} {side}", s.exchange.name()),
                amount: s.amount.map_or_else(|| DASH.to_string(), |a| format!("{} USDT", format::signed(a, 2))),
                state,
                cumulative: format!("{} USDT", format::signed(cum, 2)),
                tone,
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct PanelLine {
    pub label: String,
    pub expected: String,
    pub actual: String,
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PnlPanel {
    pub mode: String,
    /// `INCOMPLETE · reason; reason` at the top, `None` when COMPLETE.
    pub status_line: Option<String>,
    /// Expected vs actual (empty when SIMULATION: no actual column).
    pub lines: Vec<PanelLine>,
    pub safety_margin: Option<String>,
    pub settlement_note: Option<String>,
    /// Breakdown (label, value); missing parts are "—", "其他成本 0.00（未納入）".
    pub breakdown: Vec<(String, String)>,
}

fn money_or_dash(b: &PnlBreakdown, c: Component, v: Decimal) -> String {
    if b.missing.contains(&c) { DASH.to_string() } else { format!("{} USDT", format::signed(v, 2)) }
}

/// The "預期對實際" panel of a pair, `None` while no PnL exists.
pub fn pnl_panel(f: &PairFunding) -> Option<PnlPanel> {
    if f.simulated {
        return Some(PnlPanel { mode: "SIMULATION".into(), status_line: Some(SIMULATED_NO_PNL.into()), lines: Vec::new(), safety_margin: None, settlement_note: None, breakdown: Vec::new() });
    }
    let p = f.pnl.as_ref()?;
    let status_line = (p.status != "COMPLETE").then(|| format!("{} · {}", p.status, p.reasons.join("；")));
    let mut lines = Vec::new();
    let (mut safety_margin, mut settlement_note) = (None, None);
    match &p.comparison {
        Some(Comparison::Lines { lines: ls, safety_margin: sm, settlement_note: note }) => {
            for l in ls {
                let label = match l.item {
                    CompareItem::Funding => "Funding",
                    CompareItem::Fee => "手續費",
                    CompareItem::Slippage => "滑價",
                    CompareItem::Net => "Net",
                };
                let diff = match (l.diff, l.diff_pct) {
                    (Some(d), Some(pct)) => format!("{} USDT（{}%）", format::signed(d, 2), format::signed(pct, 2)),
                    (Some(d), None) => format!("{} USDT", format::signed(d, 2)),
                    (None, _) => DASH.to_string(),
                };
                lines.push(PanelLine {
                    label: label.into(),
                    expected: format!("{} USDT", format::signed(l.expected, 2)),
                    actual: l.actual.map_or_else(|| DASH.to_string(), |a| format!("{} USDT", format::signed(a, 2))),
                    diff,
                });
            }
            safety_margin = Some(format!("{} USDT（僅預期端）", format::money(*sm, 2)));
            settlement_note = note.map(|(a, e)| format!("實際結算次數 {a}，預期假設 {e}"));
        }
        Some(Comparison::NoSnapshot) | None => {
            for label in ["Funding", "手續費", "滑價", "Net"] {
                lines.push(PanelLine { label: label.into(), expected: NO_SNAPSHOT.into(), actual: DASH.into(), diff: DASH.into() });
            }
        }
    }
    let breakdown = match &p.breakdown {
        Some(b) => {
            let t = &b.total;
            vec![
                ("Funding PnL".to_string(), money_or_dash(b, Component::Funding, t.funding)),
                ("價差 PnL（參考價）".to_string(), money_or_dash(b, Component::PriceRef, t.price_ref)),
                ("開倉手續費".to_string(), money_or_dash(b, Component::OpeningFee, t.opening_fee)),
                ("平倉手續費".to_string(), money_or_dash(b, Component::ClosingFee, t.closing_fee)),
                ("滑價".to_string(), money_or_dash(b, Component::Slippage, t.slippage)),
                (
                    "其他成本".to_string(),
                    if b.other_cost_included { money_or_dash(b, Component::OtherCost, t.other_cost) } else { format!("{}（{NOT_INCLUDED}）", format::money(t.other_cost, 2)) },
                ),
                ("Net PnL".to_string(), money_or_dash(b, Component::Net, t.net)),
            ]
        }
        None => Vec::new(),
    };
    Some(PnlPanel { mode: "EXCHANGE_DEMO".into(), status_line, lines, safety_margin, settlement_note, breakdown })
}

#[cfg(test)]
#[path = "funding_tests.rs"]
mod tests;
