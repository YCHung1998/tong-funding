//! Positions (持倉) view-model (task 2.2, spec positions-page). Grouping is `core`'s
//! `group_positions` over `RECONCILED` pairs (position-grouping); filters only change what is
//! displayed, never the grouping or the header counts. The imbalance ratio is the display
//! definition of design D8 (`|Δ| ÷ max`, coin units; engine-simulation uses the same).

use std::collections::{BTreeMap, BTreeSet};

use tong_funding_core::grouping::{Grouping, PositionRow, ReconciledPair, group_positions};
use tong_funding_core::pair::PairState;
use tong_funding_core::risk::effective_for_pair;
use tong_funding_core::types::{Decimal, Exchange, Side};

use super::bridge::{AccountState, PairInfo, UiSnapshot, ACCOUNT_EXCHANGES};
use super::format::{self, DASH};
use crate::exchange::signed::models::Position;
use crate::ui::theme::Tone;

pub const PNL_NOTE: &str = "PnL 未扣手續費與資金費";
pub const OKX_NOTE: &str = "OKX 僅比價，不顯示持倉";
pub const MANUAL_NOTE: &str = "需人工處理";

/// Every position of the connected account exchanges plus `core`'s grouping of them.
/// Shared by the dashboard and the positions page so both show the same counts.
#[derive(Debug, Clone, PartialEq)]
pub struct Grouped {
    pub rows: Vec<Position>,
    /// The `RECONCILED` pairs given to `core`, in order (`groups[i].pair_index` points here).
    pub pairs: Vec<PairInfo>,
    pub grouping: Grouping,
}

impl Grouped {
    pub fn is_unpaired(&self, row: usize) -> bool {
        self.grouping.ungrouped.contains(&row)
    }
}

/// Collects positions from every `Loaded` account and groups them with `core`.
pub fn group(snap: &UiSnapshot) -> Grouped {
    let mut rows = Vec::new();
    for e in ACCOUNT_EXCHANGES {
        if let AccountState::Loaded { data, .. } = snap.account(e) {
            rows.extend(data.positions);
        }
    }
    let pairs: Vec<PairInfo> = snap.pairs.iter().filter(|p| p.state == Ok(PairState::Reconciled)).cloned().collect();
    let core_rows: Vec<PositionRow> = rows.iter().map(|p| PositionRow { exchange: p.exchange, symbol: p.symbol.clone(), payload: () }).collect();
    let core_pairs: Vec<ReconciledPair> =
        pairs.iter().map(|p| ReconciledPair { symbol: p.symbol.clone(), long_exchange: p.long_exchange, short_exchange: p.short_exchange }).collect();
    let grouping = group_positions(&core_rows, &core_pairs);
    Grouped { rows, pairs, grouping }
}

/// `|long − short| ÷ max(long, short) × 100` on absolute quantities; 0 when both are 0.
pub fn imbalance_pct(long_qty: Decimal, short_qty: Decimal) -> Decimal {
    let (l, s) = (long_qty.abs(), short_qty.abs());
    let max = l.max(s);
    if max.is_zero() { Decimal::ZERO } else { (l - s).abs() / max * Decimal::ONE_HUNDRED }
}

fn entry_notional(p: &Position) -> Option<Decimal> {
    p.entry_price.map(|e| (e * p.quantity).abs())
}

fn leg(p: &Position) -> LegView {
    LegView { exchange: p.exchange, quantity: p.quantity.abs(), entry_notional: entry_notional(p), margin: p.margin, pnl: p.unrealized_pnl }
}

fn pnl_text(long: &LegView, short: &LegView) -> String {
    let l = long.pnl.unwrap_or_default();
    let s = short.pnl.unwrap_or_default();
    format!("Pair Unrealized PnL {} USDT（{} / {}）", format::money(l + s, 2), format::signed(l, 2), format::signed(s, 2))
}

/// Cards: every grouped (`RECONCILED`) pair, then every locked pair with whatever legs exist.
fn pair_cards(snap: &UiSnapshot, g: &Grouped) -> Vec<PairCard> {
    let mut cards = Vec::new();
    for grp in &g.grouping.groups {
        let info = &g.pairs[grp.pair_index];
        let (long, short) = (leg(&g.rows[grp.long_row]), leg(&g.rows[grp.short_row]));
        let tolerance = effective_for_pair(&snap.settings.risk, &snap.settings.overrides, info.long_exchange, info.short_exchange).max_leg_imbalance_pct;
        let pct = imbalance_pct(long.quantity, short.quantity);
        let imbalanced = pct > tolerance;
        cards.push(PairCard {
            symbol: info.symbol.clone(),
            label: Some(format!("{} · {}% IMBALANCE", if imbalanced { "IMBALANCED" } else { "HEDGED" }, format::fixed(pct, 2))),
            imbalanced,
            pnl_text: Some(pnl_text(&long, &short)),
            long: Some(long),
            short: Some(short),
            state_warning: None,
        });
    }
    for p in &snap.pairs {
        let warning = match &p.state {
            Ok(st) if st.is_locked() => format!("{} · {MANUAL_NOTE}", st.as_str()),
            Err(_) => format!("狀態無法讀取 · {MANUAL_NOTE}"),
            Ok(_) => continue,
        };
        let find = |e: Exchange, side: Side| g.rows.iter().find(|r| r.exchange == e && r.symbol == p.symbol && r.side == side).map(leg);
        let (long, short) = (find(p.long_exchange, Side::Long), find(p.short_exchange, Side::Short));
        let pnl = long.as_ref().zip(short.as_ref()).map(|(l, s)| pnl_text(l, s));
        cards.push(PairCard { symbol: p.symbol.clone(), label: None, imbalanced: false, long, short, pnl_text: pnl, state_warning: Some(warning) });
    }
    cards
}

/// Display filter; `None` = everything (the default).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Filter {
    pub exchanges: Option<BTreeSet<Exchange>>,
    pub coins: Option<BTreeSet<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PosRow {
    pub exchange: Exchange,
    pub symbol: String,
    pub side: Side,
    pub quantity: Decimal,
    pub entry_price: Option<Decimal>,
    pub mark_price: Option<Decimal>,
    pub leverage: Option<Decimal>,
    pub unrealized_pnl: Option<Decimal>,
    pub unpaired: bool,
}

impl PosRow {
    /// Exchange, Symbol, Side, Size, Entry, Mark, Leverage, Unrealized PnL, Funding 收到.
    pub fn cells(&self) -> [String; 9] {
        let opt = |v: Option<Decimal>| v.map_or_else(|| DASH.to_string(), |x| format::money(x, 2));
        [
            self.exchange.name().to_string(),
            self.symbol.clone(),
            match self.side {
                Side::Long => "LONG".into(),
                Side::Short => "SHORT".into(),
            },
            format!("{} {}", format::size(self.quantity), format::base_coin(&self.symbol)),
            opt(self.entry_price),
            opt(self.mark_price),
            self.leverage.map_or_else(|| DASH.to_string(), |l| format!("{}×", l.normalize())),
            self.unrealized_pnl.map_or_else(|| DASH.to_string(), |p| format!("{} USDT", format::signed(p, 2))),
            // Funding received: filled in by `funding-pnl`; until then always a dash, never 0.00.
            DASH.to_string(),
        ]
    }
    pub fn pnl_tone(&self) -> Tone {
        match self.unrealized_pnl {
            Some(p) if p > Decimal::ZERO => Tone::Positive,
            Some(p) if p < Decimal::ZERO => Tone::Negative,
            _ => Tone::Muted,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LegView {
    pub exchange: Exchange,
    pub quantity: Decimal,
    pub entry_notional: Option<Decimal>,
    pub margin: Option<Decimal>,
    pub pnl: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PairCard {
    pub symbol: String,
    /// `HEDGED · 0.00% IMBALANCE` / `IMBALANCED · 10.00% IMBALANCE` (display only).
    pub label: Option<String>,
    pub imbalanced: bool,
    pub long: Option<LegView>,
    pub short: Option<LegView>,
    /// `Pair Unrealized PnL 0.00 USDT（+4.00 / −4.00）`.
    pub pnl_text: Option<String>,
    /// `PARTIAL_FAILURE · 需人工處理` for locked pairs (warning tone).
    pub state_warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TableView {
    Rows(Vec<PosRow>),
    /// Every option of a filter was unticked.
    NothingSelected,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PositionsVm {
    /// `4 OPEN · 2 PAIRS` (unaffected by filters).
    pub title: String,
    pub exchange_options: Vec<Exchange>,
    pub coin_options: Vec<String>,
    /// `已選 1 個交易所 / 2 個幣種 · 顯示 2 / 4`.
    pub filter_text: String,
    pub table: TableView,
    pub open_card: String,
    pub notional_total: Decimal,
    /// `BTC 2,400 + ETH 3,000 USDT`.
    pub notional_breakdown: String,
    pub pnl_total: Decimal,
    /// `Binance +4.00 · Bybit −4.00`.
    pub pnl_breakdown: String,
    pub pair_cards: Vec<PairCard>,
    /// Incomplete lists, unconnected exchanges, OKX note.
    pub notices: Vec<String>,
}

pub fn build(snap: &UiSnapshot, filter: &Filter) -> PositionsVm {
    let g = group(snap);
    let all: Vec<PosRow> = g
        .rows
        .iter()
        .enumerate()
        .map(|(i, p)| PosRow {
            exchange: p.exchange,
            symbol: p.symbol.clone(),
            side: p.side,
            quantity: p.quantity,
            entry_price: p.entry_price,
            mark_price: p.mark_price,
            leverage: p.leverage,
            unrealized_pnl: p.unrealized_pnl,
            unpaired: g.is_unpaired(i),
        })
        .collect();

    let mut exchange_options: Vec<Exchange> = all.iter().map(|r| r.exchange).collect::<BTreeSet<_>>().into_iter().collect();
    exchange_options.sort();
    let coin_options: Vec<String> = all.iter().map(|r| format::base_coin(&r.symbol).to_string()).collect::<BTreeSet<_>>().into_iter().collect();
    let sel_ex: BTreeSet<Exchange> = filter.exchanges.clone().unwrap_or_else(|| exchange_options.iter().copied().collect());
    let sel_coin: BTreeSet<String> = filter.coins.clone().unwrap_or_else(|| coin_options.iter().cloned().collect());
    let shown: Vec<PosRow> =
        all.iter().filter(|r| sel_ex.contains(&r.exchange) && sel_coin.contains(format::base_coin(&r.symbol))).cloned().collect();
    let selected_ex = exchange_options.iter().filter(|e| sel_ex.contains(e)).count();
    let selected_coin = coin_options.iter().filter(|c| sel_coin.contains(*c)).count();
    let filter_text = format!("已選 {selected_ex} 個交易所 / {selected_coin} 個幣種 · 顯示 {} / {}", shown.len(), all.len());
    let nothing_selected = (filter.exchanges.as_ref().is_some_and(|s| s.is_empty()) && !exchange_options.is_empty())
        || (filter.coins.as_ref().is_some_and(|s| s.is_empty()) && !coin_options.is_empty());
    let table = if nothing_selected { TableView::NothingSelected } else { TableView::Rows(shown) };

    let pairs = g.grouping.groups.len();
    let mut by_coin: BTreeMap<String, Decimal> = BTreeMap::new();
    let mut coin_order: Vec<String> = Vec::new();
    let mut by_ex: BTreeMap<Exchange, Decimal> = BTreeMap::new();
    for p in &g.rows {
        let coin = format::base_coin(&p.symbol).to_string();
        if !coin_order.contains(&coin) {
            coin_order.push(coin.clone());
        }
        *by_coin.entry(coin).or_default() += entry_notional(p).unwrap_or_default();
        *by_ex.entry(p.exchange).or_default() += p.unrealized_pnl.unwrap_or_default();
    }
    let notional_total: Decimal = by_coin.values().copied().sum();
    let pnl_total: Decimal = by_ex.values().copied().sum();
    let notional_breakdown = if coin_order.is_empty() {
        DASH.to_string()
    } else {
        format!("{} USDT", coin_order.iter().map(|c| format!("{c} {}", format::compact(by_coin[c]))).collect::<Vec<_>>().join(" + "))
    };
    let pnl_breakdown = if by_ex.is_empty() {
        DASH.to_string()
    } else {
        by_ex.iter().map(|(e, v)| format!("{} {}", e.name(), format::signed(*v, 2))).collect::<Vec<_>>().join(" · ")
    };

    let mut notices = Vec::new();
    for e in ACCOUNT_EXCHANGES {
        match snap.account(e) {
            AccountState::Loaded { data, .. } if data.positions_incomplete.is_some() => notices.push(format!("{} 持倉列表可能不完整", e.name())),
            AccountState::NotConnected { reason } => notices.push(format!("{} 未連線：{reason}", e.name())),
            AccountState::Failed { error, .. } => notices.push(format!("{} 持倉讀取失敗：{error}", e.name())),
            AccountState::Loading => notices.push(format!("{} 持倉載入中", e.name())),
            _ => {}
        }
    }
    notices.push(OKX_NOTE.to_string());

    PositionsVm {
        title: format!("{} OPEN · {pairs} PAIRS", all.len()),
        exchange_options,
        coin_options,
        filter_text,
        table,
        open_card: format!("{} · 與總覽一致 · {pairs} 對 / {} 腿", all.len(), all.len()),
        notional_total,
        notional_breakdown,
        pnl_total,
        pnl_breakdown,
        pair_cards: pair_cards(snap, &g),
        notices,
    }
}

#[cfg(test)]
#[path = "positions_tests.rs"]
mod tests;
