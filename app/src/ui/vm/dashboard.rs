//! Dashboard (總覽) view-model (task 2.1, spec dashboard-page). Valuation rule (design D9): an
//! exchange's Value is wallet assets (in USDT) + contract equity; position notional is never
//! counted. Unconnected exchanges (OKX included) are never treated as zero.

use std::collections::BTreeMap;

use tong_funding_core::types::{Decimal, Exchange};

use super::bridge::{tradable_exchanges, AccountData, AccountState, ContractEquity, UiSnapshot};
use super::disabled::{self, DisabledStatus};
use super::format::{self, DASH};
use super::labels::{disabled_exchange_line, plain_error, DisabledNote, DISABLED_WITH_EXPOSURE};
use super::positions::{Grouped, group};
use crate::exchange::signed::models::Position;
use crate::ui::theme::Tone;

pub const CONTRACT_EQUITY_LABEL: &str = "合約";
pub const EQUITY_FOOTNOTE: &str = "合約權益含已用與可用保證金，未重複計入合約名目本金";

/// One row of an exchange's asset table.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetRow {
    pub name: String,
    pub price: Option<Decimal>,
    pub quantity: Option<Decimal>,
    /// `None` = cannot be valued (shown as 無法估值, not counted).
    pub value: Option<Decimal>,
    /// Share of the exchange Value (full precision; displayed with 4 decimals).
    pub pct: Option<Decimal>,
}

impl AssetRow {
    pub fn price_text(&self) -> String {
        self.price.map_or_else(|| DASH.into(), |p| format::money(p, 2))
    }
    pub fn quantity_text(&self) -> String {
        self.quantity.map_or_else(|| DASH.into(), |q| q.normalize().to_string())
    }
    pub fn value_text(&self) -> String {
        self.value.map_or_else(|| "無法估值".into(), |v| format::money(v, 2))
    }
    pub fn pct_text(&self) -> String {
        self.pct.map_or_else(|| DASH.into(), |p| format!("{}%", format::fixed(p, 4)))
    }
}

/// One slice of the margin donut.
#[derive(Debug, Clone, PartialEq)]
pub struct MarginSlice {
    pub label: String,
    pub margin: Decimal,
    pub pct: Decimal,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarginDistribution {
    NoPositions,
    Slices {
        used_total: Decimal,
        slices: Vec<MarginSlice>,
        /// At least one margin was estimated as notional ÷ leverage.
        estimated: bool,
        /// Positions whose margin could not be determined at all (not counted).
        unknown: usize,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConnectedCard {
    pub exchange: Exchange,
    /// `● CONNECTED · DEMO` (or TESTNET).
    pub status_label: String,
    pub value: Decimal,
    /// Share of the total portfolio (2 decimals when shown).
    pub pct_of_total: Option<Decimal>,
    pub assets: Vec<AssetRow>,
    /// Count of 無法估值 rows.
    pub unvalued: usize,
    pub margin: MarginDistribution,
    /// `45 秒前的資料 · 可能已過期` when the latest poll failed.
    pub stale_note: Option<String>,
    /// The exchange is not in `allowed_exchanges` but still holds positions: its card stays and says so.
    pub disabled_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExchangeCard {
    Connected(ConnectedCard),
    /// `disabled_note` (here and on `Loading` and `Failed`): the exchange is not in `allowed_exchanges`
    /// but still has exposure (positions or an unfinished pair) while its read is not good, so the
    /// card stays with this warning instead of folding into a line.
    NotConnected { exchange: Exchange, reason: String, disabled_note: Option<String> },
    Loading { exchange: Exchange, disabled_note: Option<String> },
    /// `error` is the message as the exchange adapter said it; `plain` is the same in plain words.
    Failed { exchange: Exchange, error: String, plain: String, at: i64, disabled_note: Option<String> },
    /// Not in `allowed_exchanges` and nothing to show: one muted line, the state behind it.
    Disabled(DisabledNote),
}

#[derive(Debug, Clone, PartialEq)]
pub struct DashboardVm {
    pub total: Decimal,
    /// The exchanges the total counts, `Binance + Bybit + OKX`; `尚無已連線的交易所` when none is.
    pub exchanges_text: String,
    /// e.g. `不含未連線的交易所：Bybit`.
    pub excluded_note: Option<String>,
    /// `不含 N 項無法估值的資產`.
    pub unvalued_note: Option<String>,
    pub cards: Vec<ExchangeCard>,
    pub open_positions: usize,
    /// `2 對避險組合 · 4 個交易所倉位`.
    pub positions_note: String,
    /// The exposure line, and whether it must be shown in the warning tone.
    pub exposure: String,
    pub unhedged: usize,
    pub exposure_tone: Tone,
    /// `帳戶刷新 12s`.
    pub refresh_text: Option<String>,
}

impl DashboardVm {
    pub fn pct_text(card: &ConnectedCard) -> String {
        card.pct_of_total.map_or_else(|| DASH.into(), |p| format!("{}%", format::fixed(p, 2)))
    }
}

/// Values the asset rows of one account (pure).
pub fn asset_rows(data: &AccountData) -> (Vec<AssetRow>, Decimal, usize) {
    let mut rows = Vec::new();
    for a in data.assets.iter().filter(|a| !a.quantity.is_zero()) {
        let (price, value) = if a.asset.eq_ignore_ascii_case("USDT") {
            (Some(Decimal::ONE), Some(a.quantity))
        } else if let Some(v) = a.usdt_value {
            (Some(v / a.quantity), Some(v))
        } else if let Some(m) = a.mark_price {
            (Some(m), Some(m * a.quantity))
        } else {
            (None, None)
        };
        rows.push(AssetRow { name: a.asset.clone(), price, quantity: Some(a.quantity), value, pct: None });
    }
    if let Some(ContractEquity { used_margin, available_margin }) = data.contract_equity {
        rows.push(AssetRow { name: CONTRACT_EQUITY_LABEL.into(), price: None, quantity: None, value: Some(used_margin + available_margin), pct: None });
    }
    let total: Decimal = rows.iter().filter_map(|r| r.value).sum();
    for r in &mut rows {
        r.pct = r.value.and_then(|v| format::share_pct(v, total));
    }
    let unvalued = rows.iter().filter(|r| r.value.is_none()).count();
    (rows, total, unvalued)
}

/// A position's initial margin: the exchange's value, else notional ÷ leverage (estimated).
fn initial_margin(p: &Position) -> Option<(Decimal, bool)> {
    if let Some(m) = p.margin {
        return Some((m, false));
    }
    let notional = p.notional.map(|n| n.abs()).or_else(|| p.mark_price.or(p.entry_price).map(|px| (px * p.quantity).abs()))?;
    let lev = p.leverage.filter(|l| *l > Decimal::ZERO)?;
    Some((notional / lev, true))
}

/// The margin distribution of one exchange's positions (pure).
pub fn margin_distribution(positions: &[Position]) -> MarginDistribution {
    if positions.is_empty() {
        return MarginDistribution::NoPositions;
    }
    let mut known = Vec::new();
    let mut estimated = false;
    let mut unknown = 0;
    for p in positions {
        match initial_margin(p) {
            Some((m, est)) => {
                estimated |= est;
                known.push((format::base_coin(&p.symbol).to_string(), m));
            }
            None => unknown += 1,
        }
    }
    let used_total: Decimal = known.iter().map(|(_, m)| *m).sum();
    let slices = known
        .into_iter()
        .map(|(label, margin)| MarginSlice { pct: format::share_pct(margin, used_total).unwrap_or_default(), label, margin })
        .collect();
    MarginDistribution::Slices { used_total, slices, estimated, unknown }
}

fn used_margin(m: &MarginDistribution) -> Decimal {
    match m {
        MarginDistribution::NoPositions => Decimal::ZERO,
        MarginDistribution::Slices { used_total, .. } => *used_total,
    }
}

/// `BTC 與 ETH 各 1 對中性組合`, `BTC 2 對、ETH 1 對中性組合`, `0 對中性組合`.
fn pairs_phrase(g: &Grouped) -> String {
    let mut by_coin: BTreeMap<String, usize> = BTreeMap::new();
    for grp in &g.grouping.groups {
        *by_coin.entry(format::base_coin(&g.pairs[grp.pair_index].symbol).to_string()).or_default() += 1;
    }
    let counts: Vec<usize> = by_coin.values().copied().collect();
    match counts.as_slice() {
        [] => "0 對中性組合".into(),
        [n] => format!("{} {n} 對中性組合", by_coin.keys().next().cloned().unwrap_or_default()),
        [first, rest @ ..] if rest.iter().all(|n| n == first) => {
            format!("{} 各 {first} 對中性組合", by_coin.keys().cloned().collect::<Vec<_>>().join(" 與 "))
        }
        _ => format!("{}中性組合", by_coin.iter().map(|(c, n)| format!("{c} {n} 對")).collect::<Vec<_>>().join("、")),
    }
}

pub fn build(snap: &UiSnapshot, now_ms: i64) -> DashboardVm {
    let grouped = group(snap);
    let mut cards = Vec::new();
    let mut excluded = Vec::new();
    // unticked exchanges whose positions cannot be confirmed (named in the exposure summary)
    let mut unconfirmed: Vec<&str> = Vec::new();
    let mut unvalued_total = 0;
    let mut used_total = Decimal::ZERO;
    let mut pnl_total = Decimal::ZERO;
    for e in tradable_exchanges(snap.identity).iter().copied() {
        // ui-plain-language: an exchange the risk settings left unticked is one line ONLY when nothing
        // about its exposure is open: muted when the latest read found nothing, in the warning tone
        // when the positions cannot be confirmed. Positions, an incomplete list or an unfinished pair
        // keep the card, with a note (exposure is never hidden).
        let verdict = (!snap.exchange_enabled(e)).then(|| disabled::status(snap, e, now_ms));
        let exposed = verdict == Some(DisabledStatus::Exposed);
        let folded = |detail: String| -> ExchangeCard {
            ExchangeCard::Disabled(match &verdict {
                Some(DisabledStatus::Unconfirmed { reason }) => DisabledNote::unconfirmed(e, reason, Some(detail)),
                _ => DisabledNote::new(e, Some(detail)),
            })
        };
        let card_note = exposed.then(|| format!("{} · {DISABLED_WITH_EXPOSURE}", disabled_exchange_line(e)));
        let fold = verdict.is_some() && !exposed;
        if matches!(verdict, Some(DisabledStatus::Unconfirmed { .. })) {
            // its value is not in the total and its positions are not known: say so in the notes
            excluded.push(e.name());
            unconfirmed.push(e.name());
        }
        let account = snap.account(e);
        if fold {
            // an unticked exchange without exposure: one muted line, the page it would have drawn behind it
            let detail = match account {
                AccountState::Loaded { data, fetched_at, .. } => {
                    let value = asset_rows(&data).1;
                    match verdict {
                        Some(DisabledStatus::Unconfirmed { .. }) => format!("最後一次成功讀取是 {} 秒前；資產 {} USDT 未計入總資產", format::secs(now_ms - fetched_at), format::money(value, 2)),
                        _ => {
                            let age = disabled::read_age_note(snap, e, now_ms).map(|a| format!("（{a}）")).unwrap_or_default();
                            format!("已連線，沒有持倉{age}；資產 {} USDT 未計入總資產", format::money(value, 2))
                        }
                    }
                }
                AccountState::NotConnected { reason } => format!("未連線：{reason}"),
                AccountState::Failed { error, at } => {
                    let plain = plain_error(&error);
                    let raw = if plain == error { String::new() } else { format!(" · 原始訊息：{error}") };
                    format!("載入失敗：{plain}（{} UTC）{raw}", format::utc_hms(at))
                }
                AccountState::Loading => "載入中".into(),
            };
            cards.push(folded(detail));
            continue;
        }
        match account {
            AccountState::Loaded { data, fetched_at, error } => {
                let (assets, value, unvalued) = asset_rows(&data);
                let margin = margin_distribution(&data.positions);
                used_total += used_margin(&margin);
                pnl_total += data.positions.iter().filter_map(|p| p.unrealized_pnl).sum::<Decimal>();
                unvalued_total += unvalued;
                let stale_note = error.filter(|(_, at)| *at >= fetched_at).map(|_| format!("{} 秒前的資料 · 可能已過期", format::secs(now_ms - fetched_at)));
                cards.push(ExchangeCard::Connected(ConnectedCard {
                    exchange: e,
                    status_label: format!("● CONNECTED · {}", data.environment),
                    value,
                    pct_of_total: None,
                    assets,
                    unvalued,
                    margin,
                    stale_note,
                    disabled_note: card_note,
                }));
            }
            AccountState::NotConnected { reason } => {
                excluded.push(e.name());
                cards.push(ExchangeCard::NotConnected { exchange: e, reason, disabled_note: card_note });
            }
            AccountState::Failed { error, at } => {
                let plain = plain_error(&error);
                excluded.push(e.name());
                cards.push(ExchangeCard::Failed { exchange: e, error, plain, at, disabled_note: card_note });
            }
            AccountState::Loading => {
                excluded.push(e.name());
                cards.push(ExchangeCard::Loading { exchange: e, disabled_note: card_note });
            }
        }
    }

    let total: Decimal = cards.iter().filter_map(|c| if let ExchangeCard::Connected(c) = c { Some(c.value) } else { None }).sum();
    for c in &mut cards {
        if let ExchangeCard::Connected(c) = c {
            c.pct_of_total = format::share_pct(c.value, total);
        }
    }

    // The description lists the exchanges the total counts (the connected cards), nothing else.
    let counted: Vec<&str> = cards.iter().filter_map(|c| if let ExchangeCard::Connected(c) = c { Some(c.exchange.name()) } else { None }).collect();
    let exchanges_text = if counted.is_empty() { "尚無已連線的交易所".to_string() } else { counted.join(" + ") };

    let open_positions = grouped.rows.len();
    let pairs = grouped.grouping.groups.len();
    let unhedged = grouped.grouping.ungrouped.len();
    let legs = if unhedged == 0 { "無未避險單腿".to_string() } else { format!("{unhedged} 個未避險單腿") };
    let unconfirmed_note = if unconfirmed.is_empty() { String::new() } else { format!("（{} 持倉無法確認）", unconfirmed.join("、")) };
    let exposure = format!(
        "{} · 已用保證金 {} USDT · 未實現 PnL {} USDT · {legs}{unconfirmed_note}",
        pairs_phrase(&grouped),
        format::money(used_total, 2),
        format::money(pnl_total, 2)
    );
    let refresh_text = tradable_exchanges(snap.identity)
        .iter()
        .filter_map(|e| snap.health_of(super::bridge::SourceId::Account(*e)).and_then(|h| h.next_poll_at))
        .min()
        .map(|t| format!("帳戶刷新 {}s", format::secs(t - now_ms)));

    DashboardVm {
        total,
        exchanges_text,
        excluded_note: (!excluded.is_empty()).then(|| format!("不含未連線的交易所：{}", excluded.join("、"))),
        unvalued_note: (unvalued_total > 0).then(|| format!("不含 {unvalued_total} 項無法估值的資產")),
        cards,
        open_positions,
        positions_note: format!("{pairs} 對避險組合 · {open_positions} 個交易所倉位"),
        exposure,
        unhedged,
        exposure_tone: if unhedged > 0 || !unconfirmed.is_empty() { Tone::Warning } else { Tone::Muted },
        refresh_text,
    }
}

#[cfg(test)]
#[path = "dashboard_tests.rs"]
mod tests;
