//! GPUI drawing of the read-only pages and the alert banner (ui-readonly-pages). Thin by design
//! (design D1): every number, label and decision comes from the view-models in `ui::<page>`;
//! this file only lays them out with theme tokens. Verified by screenshots (task 4.1).

use std::collections::BTreeMap;

use gpui_kit::component::chart::PieChart;
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::*;
use tong_funding_core::types::{Decimal, Exchange};

use super::banner::{BannerVm, Freshness, LoadView};
use super::bridge::ClockState;
use super::dashboard::{self, ConnectedCard, DashboardVm, ExchangeCard, MarginDistribution};
use super::format::{self, DASH};
use super::positions::{PairCard, PositionsVm, TableView};
use super::scanner::{self, RateCell, ScanRow};
use super::system_log::{SystemLogVm, Timeline};
use super::theme::{self, Tone};

pub fn text(s: impl Into<SharedString>, color: u32) -> Div {
    div().text_color(rgb(color)).child(s.into())
}

pub fn small(s: impl Into<SharedString>, color: u32) -> Div {
    text(s, color).text_size(px(10.0))
}

pub fn card() -> Div {
    div().p_3().rounded_md().bg(rgb(theme::BG_CARD)).border_1().border_color(rgb(theme::BORDER)).flex().flex_col().gap_1()
}

pub fn title(zh: &str, en: &str) -> Div {
    div()
        .flex()
        .items_end()
        .gap_3()
        .child(text(zh.to_string(), theme::TEXT_PRIMARY).text_size(px(21.0)))
        .child(text(en.to_string(), theme::TEXT_SECONDARY))
}

fn tone(t: Tone) -> u32 {
    t.color()
}

/// Loading / failed / empty placeholder of a block.
pub fn load_state(v: &LoadView) -> Option<Div> {
    v.text().map(|t| {
        let color = if matches!(v, LoadView::Failed { .. }) { theme::NEGATIVE } else { theme::TEXT_MUTED };
        div().p_4().child(text(t, color))
    })
}

pub fn freshness_chip(label: &str, f: &Freshness) -> Div {
    let color = match f.status {
        super::alerts::FreshStatus::Online => theme::POSITIVE,
        super::alerts::FreshStatus::RateLimited | super::alerts::FreshStatus::Stale => theme::WARNING,
        super::alerts::FreshStatus::Offline | super::alerts::FreshStatus::Unknown => theme::NEGATIVE,
    };
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(div().size(px(6.0)).rounded_full().bg(rgb(color)))
        .child(small(label.to_string(), theme::TEXT_SECONDARY))
        .child(small(f.text(), theme::TEXT_MUTED))
}

// ---- banner ---------------------------------------------------------------------------------

/// The banner; `None` when there is nothing to show (takes no space).
pub fn banner<F, G>(vm: &BannerVm, mut on_dismiss: F, mut on_link: G) -> Option<Div>
where
    F: FnMut(usize) -> Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
    G: FnMut(usize) -> Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
{
    if !vm.visible() {
        return None;
    }
    let mut col = div().flex().flex_col().px_4().py_1().gap_1().bg(rgb(theme::BG_SURFACE)).border_b_1().border_color(rgb(theme::BORDER));
    for (i, item) in vm.items.iter().enumerate() {
        let color = if item.severe { theme::NEGATIVE } else { theme::WARNING };
        let mut row = div()
            .flex()
            .items_center()
            .gap_3()
            .child(div().px_2().rounded_sm().bg(rgb(theme::BG_CARD)).child(small(item.category, color)))
            .child(text(item.message.clone(), theme::TEXT_PRIMARY).flex_1());
        if let Some(age) = &item.age_text {
            row = row.child(small(age.clone(), theme::TEXT_MUTED));
        }
        if let Some((label, _)) = item.link {
            row = row.child(div().id(("banner-link", i)).cursor_pointer().on_click(on_link(i)).child(small(format!("{label} →"), theme::ACCENT)));
        }
        if item.closable {
            row = row.child(div().id(("banner-close", i)).cursor_pointer().on_click(on_dismiss(i)).child(small("×", theme::TEXT_MUTED)));
        }
        col = col.child(row);
    }
    Some(col)
}

// ---- dashboard ------------------------------------------------------------------------------

const SLICE_COLORS: [u32; 5] = [theme::ACCENT, theme::WARNING, theme::INFO, theme::POSITIVE, theme::NEGATIVE];

#[derive(Clone)]
struct Slice {
    value: f32,
    color: u32,
}

fn as_f32(d: Decimal) -> f32 {
    d.to_string().parse().unwrap_or(0.0)
}

fn donut(values: Vec<Decimal>) -> Div {
    let data: Vec<Slice> = values.iter().enumerate().map(|(i, v)| Slice { value: as_f32(*v), color: SLICE_COLORS[i % SLICE_COLORS.len()] }).collect();
    div().w(px(140.0)).h(px(140.0)).child(PieChart::new(data).value(|s: &Slice| s.value).color(|s: &Slice| rgb(s.color)).inner_radius(38.0).outer_radius(62.0))
}

fn legend(labels: Vec<(String, String)>) -> Div {
    let mut col = div().flex().flex_col().gap_1();
    for (i, (name, pct)) in labels.into_iter().enumerate() {
        col = col.child(
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(div().size(px(8.0)).rounded_sm().bg(rgb(SLICE_COLORS[i % SLICE_COLORS.len()])))
                .child(small(name, theme::TEXT_SECONDARY))
                .child(small(pct, theme::TEXT_MUTED)),
        );
    }
    col
}

fn connected_card(c: &ConnectedCard) -> Div {
    let header = div()
        .flex()
        .justify_between()
        .child(text(c.exchange.name(), theme::TEXT_PRIMARY).text_size(px(14.0)))
        .child(small(c.status_label.clone(), theme::POSITIVE));
    let total = div()
        .flex()
        .gap_2()
        .items_end()
        .child(small("EXCHANGE TOTAL", theme::TEXT_MUTED))
        .child(text(format!("{} USDT", format::money(c.value, 2)), theme::TEXT_PRIMARY).text_size(px(14.0)))
        .child(small(DashboardVm::pct_text(c), theme::TEXT_SECONDARY));
    let valued: Vec<_> = c.assets.iter().filter(|a| a.value.is_some()).collect();
    let asset_chart = div()
        .flex()
        .gap_2()
        .child(donut(valued.iter().filter_map(|a| a.value).collect()))
        .child(legend(valued.iter().map(|a| (a.name.clone(), a.pct_text())).collect()));
    let margin_chart = match &c.margin {
        MarginDistribution::NoPositions => div().child(small("無持倉", theme::TEXT_MUTED)),
        MarginDistribution::Slices { used_total, slices, estimated, unknown } => {
            let mut note = format!("已用保證金 {} USDT", format::money(*used_total, 2));
            if *estimated {
                note.push_str(" · 估算");
            }
            if *unknown > 0 {
                note.push_str(&format!(" · {unknown} 筆無法計算"));
            }
            div()
                .flex()
                .gap_2()
                .child(donut(slices.iter().map(|s| s.margin).collect()))
                .child(legend(slices.iter().map(|s| (s.label.clone(), format!("{}%", format::fixed(s.pct, 2)))).collect()).child(small(note, theme::TEXT_MUTED)))
        }
    };
    let mut table = div().flex().flex_col().child(
        div().flex().gap_2().children(["Asset", "Price", "Quantity", "Value (USDT)", "% of Exchange"].map(|h| small(h, theme::TEXT_MUTED).w(px(110.0)))),
    );
    for a in &c.assets {
        table = table.child(div().flex().gap_2().children(
            [a.name.clone(), a.price_text(), a.quantity_text(), a.value_text(), a.pct_text()].map(|s| small(s, theme::TEXT_SECONDARY).w(px(110.0))),
        ));
    }
    table = table
        .child(div().flex().gap_2().children(
            ["合計".to_string(), String::new(), String::new(), format::money(c.value, 2), "100.0000%".into()].map(|s| small(s, theme::TEXT_PRIMARY).w(px(110.0))),
        ))
        .child(small(dashboard::EQUITY_FOOTNOTE, theme::TEXT_MUTED));
    let mut out = card().w(px(560.0)).child(header).child(total).child(div().flex().gap_6().child(asset_chart).child(margin_chart)).child(table);
    if let Some(n) = &c.stale_note {
        out = out.child(small(n.clone(), theme::WARNING));
    }
    out
}

pub fn dashboard_page(vm: &DashboardVm) -> Div {
    let mut total = card()
        .child(small("Total Portfolio Value", theme::TEXT_MUTED))
        .child(text(format!("{} USDT", format::money(vm.total, 2)), theme::TEXT_PRIMARY).text_size(px(21.0)))
        .child(small("Binance + Bybit · OKX 僅比價", theme::TEXT_SECONDARY));
    for n in [&vm.excluded_note, &vm.unvalued_note].into_iter().flatten() {
        total = total.child(small(n.clone(), theme::WARNING));
    }
    let mut top = div().flex().gap_3().child(total);
    for c in &vm.cards {
        if let ExchangeCard::Connected(c) = c {
            top = top.child(
                card()
                    .child(small(format!("{} Value", c.exchange.name()), theme::TEXT_MUTED))
                    .child(text(format::money(c.value, 2), theme::TEXT_PRIMARY).text_size(px(14.0)))
                    .child(small(DashboardVm::pct_text(c), theme::TEXT_SECONDARY)),
            );
        }
    }
    top = top.child(
        card()
            .child(small("Open Positions", theme::TEXT_MUTED))
            .child(text(vm.open_positions.to_string(), theme::TEXT_PRIMARY).text_size(px(14.0)))
            .child(small(vm.positions_note.clone(), theme::TEXT_SECONDARY)),
    );
    let mut cards = div().flex().flex_wrap().gap_3();
    for c in &vm.cards {
        cards = cards.child(match c {
            ExchangeCard::Connected(c) => connected_card(c),
            ExchangeCard::NotConnected { exchange, reason } => card().child(text(exchange.name(), theme::TEXT_PRIMARY)).child(small(format!("● 未連線 · {reason}"), theme::NEGATIVE)),
            ExchangeCard::Loading { exchange } => card().child(text(exchange.name(), theme::TEXT_PRIMARY)).child(small("載入中", theme::TEXT_MUTED)),
            ExchangeCard::Failed { exchange, error, at } => card()
                .child(text(exchange.name(), theme::TEXT_PRIMARY))
                .child(small(format!("載入失敗：{error}（{} UTC）", format::utc_hms(*at)), theme::NEGATIVE)),
            ExchangeCard::CompareOnly { exchange } => card().child(text(exchange.name(), theme::TEXT_PRIMARY)).child(small(dashboard::OKX_NOTE, theme::TEXT_MUTED)),
        });
    }
    let mut header = div().flex().justify_between().child(title("總覽", "Dashboard"));
    if let Some(r) = &vm.refresh_text {
        header = header.child(small(r.clone(), theme::TEXT_MUTED));
    }
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(header)
        .child(top)
        .child(cards)
        .child(card().child(small("曝險摘要", theme::TEXT_MUTED)).child(text(vm.exposure.clone(), tone(vm.exposure_tone))))
}

// ---- positions ------------------------------------------------------------------------------

fn pair_card(c: &PairCard) -> Div {
    let mut out = card().w(px(360.0)).child(
        div()
            .flex()
            .justify_between()
            .child(text(format!("{} · 中性組合", c.symbol), theme::TEXT_PRIMARY))
            .child(match (&c.state_warning, &c.label) {
                (Some(w), _) => small(w.clone(), theme::WARNING),
                (None, Some(l)) => small(l.clone(), if c.imbalanced { theme::WARNING } else { theme::POSITIVE }),
                (None, None) => small(DASH, theme::TEXT_MUTED),
            }),
    );
    for (side, leg) in [("LONG", &c.long), ("SHORT", &c.short)] {
        out = out.child(match leg {
            Some(l) => small(
                format!(
                    "{side} · {} · {} · Notional {} · Margin {}",
                    l.exchange.name(),
                    format::size(l.quantity),
                    l.entry_notional.map_or_else(|| DASH.into(), |v| format::money(v, 2)),
                    l.margin.map_or_else(|| DASH.into(), |v| format::money(v, 2))
                ),
                theme::TEXT_SECONDARY,
            ),
            None => small(format!("{side} · 找不到持倉"), theme::WARNING),
        });
    }
    if let Some(p) = &c.pnl_text {
        out = out.child(small(p.clone(), theme::TEXT_PRIMARY));
    }
    // funding-pnl (settlement-timeline): funding received, running total, timeline, panel.
    let f = &c.funding;
    out = out.child(small(f.text.clone(), tone(f.tone))).child(small(f.running_total.clone(), theme::TEXT_SECONDARY));
    if !f.timeline.is_empty() {
        out = out.child(small("結算時間軸", theme::TEXT_MUTED));
        for r in &f.timeline {
            out = out.child(small(format!("{} · {} · {} · {} · 累計 {}", r.time, r.leg, r.amount, r.state, r.cumulative), tone(r.tone)));
        }
    }
    if let Some(p) = &f.panel {
        out = out.child(small(format!("預期對實際 · {}", p.mode), theme::TEXT_MUTED));
        if let Some(s) = &p.status_line {
            out = out.child(small(s.clone(), theme::WARNING));
        }
        for l in &p.lines {
            out = out.child(small(format!("{} · 預期 {} · 實際 {} · 差異 {}", l.label, l.expected, l.actual, l.diff), theme::TEXT_SECONDARY));
        }
        if let Some(s) = &p.safety_margin {
            out = out.child(small(format!("安全邊際 {s}"), theme::TEXT_MUTED));
        }
        if let Some(s) = &p.settlement_note {
            out = out.child(small(s.clone(), theme::WARNING));
        }
        for (k, v) in &p.breakdown {
            out = out.child(small(format!("{k} {v}"), theme::TEXT_SECONDARY));
        }
    }
    for (_, a) in &f.alerts {
        out = out.child(small(a.clone(), theme::WARNING));
    }
    out
}

pub type ClickFn = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

pub fn positions_page(vm: &PositionsVm, selected_ex: &dyn Fn(Exchange) -> bool, selected_coin: &dyn Fn(&str) -> bool, on_ex: &dyn Fn(Exchange) -> ClickFn, on_coin: &dyn Fn(String) -> ClickFn) -> Div {
    let chip = |id: SharedString, label: String, on: bool, f: ClickFn| {
        div()
            .id(id)
            .px_2()
            .rounded_sm()
            .cursor_pointer()
            .bg(rgb(if on { theme::BG_CARD } else { theme::BG_BASE }))
            .border_1()
            .border_color(rgb(if on { theme::ACCENT } else { theme::BORDER }))
            .on_click(f)
            .child(small(format!("{} {label}", if on { "☑" } else { "☐" }), theme::TEXT_SECONDARY))
    };
    let mut filters = div().flex().gap_2().items_center().child(small("Exchange", theme::TEXT_MUTED));
    for e in &vm.exchange_options {
        filters = filters.child(chip(format!("ex-{}", e.name()).into(), e.name().to_string(), selected_ex(*e), on_ex(*e)));
    }
    filters = filters.child(small("Coin", theme::TEXT_MUTED));
    for c in &vm.coin_options {
        filters = filters.child(chip(format!("coin-{c}").into(), c.clone(), selected_coin(c), on_coin(c.clone())));
    }
    filters = filters.child(small(vm.filter_text.clone(), theme::TEXT_SECONDARY));

    let cards = div()
        .flex()
        .gap_3()
        .child(card().child(small("Open Positions", theme::TEXT_MUTED)).child(text(vm.open_card.clone(), theme::TEXT_PRIMARY)))
        .child(
            card()
                .child(small("Entry Notional", theme::TEXT_MUTED))
                .child(text(format!("{} USDT", format::money(vm.notional_total, 2)), theme::TEXT_PRIMARY))
                .child(small(vm.notional_breakdown.clone(), theme::TEXT_SECONDARY)),
        )
        .child(
            card()
                .child(small("Unrealized PnL", theme::TEXT_MUTED))
                .child(text(format!("{} USDT", format::signed(vm.pnl_total, 2)), theme::TEXT_PRIMARY))
                .child(small(vm.pnl_breakdown.clone(), theme::TEXT_SECONDARY)),
        );

    const HEAD: [&str; 9] = ["Exchange", "Symbol", "Side", "Size", "Entry Price", "Mark Price", "Leverage", "Unrealized PnL", "Funding 收到"];
    let mut table = div().flex().flex_col().gap_1().child(div().flex().gap_2().children(HEAD.map(|h| small(h, theme::TEXT_MUTED).w(px(120.0)))));
    match &vm.table {
        TableView::NothingSelected => table = table.child(text("未選擇任何篩選條件", theme::TEXT_MUTED)),
        TableView::Rows(rows) if rows.is_empty() => table = table.child(text("沒有持倉", theme::TEXT_MUTED)),
        TableView::Rows(rows) => {
            for r in rows {
                let cells = r.cells();
                let mut line = div().flex().gap_2();
                for (i, c) in cells.into_iter().enumerate() {
                    let color = match i {
                        7 => tone(r.pnl_tone()),
                        8 => tone(r.funding_tone),
                        _ => theme::TEXT_SECONDARY,
                    };
                    line = line.child(small(c, color).w(px(120.0)));
                }
                if r.unpaired {
                    line = line.child(small("未配對", theme::WARNING));
                }
                table = table.child(line);
            }
        }
    }
    table = table.child(small(super::positions::PNL_NOTE, theme::TEXT_MUTED));
    let mut notices = div().flex().flex_col();
    for n in &vm.notices {
        notices = notices.child(small(n.clone(), theme::WARNING));
    }
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(div().flex().justify_between().child(title("持倉", "Unified Positions")).child(small(vm.title.clone(), theme::TEXT_SECONDARY)))
        .child(filters)
        .child(cards)
        .child(notices)
        .child(card().child(table))
        .child(div().flex().flex_wrap().gap_3().children(vm.pair_cards.iter().map(pair_card)))
}

// ---- system log -----------------------------------------------------------------------------

pub fn system_log_page(
    vm: Option<&SystemLogVm>,
    error: Option<&LoadView>,
    selected: &dyn Fn(&str) -> bool,
    on_type: &dyn Fn(String) -> ClickFn,
    on_older: ClickFn,
) -> Div {
    let mut page = div().flex().flex_col().gap_3().child(div().flex().justify_between().child(title("系統日誌", "System Logs")).child(small("READ ONLY", theme::TEXT_MUTED)));
    if let Some(e) = error.and_then(load_state) {
        page = page.child(e);
    }
    let Some(vm) = vm else { return page };
    let mut filters = div().flex().flex_wrap().gap_2().items_center();
    for t in &vm.type_options {
        let on = selected(t);
        let mut label = format!("{} {t}", if on { "☑" } else { "☐" });
        if t == crate::store::events::SCAN_RUN {
            label.push_str(&format!("（{}）", super::system_log::SCAN_RUN_NOTE));
        }
        filters = filters.child(div().id(SharedString::from(format!("type-{t}"))).px_2().rounded_sm().cursor_pointer().bg(rgb(theme::BG_CARD)).on_click(on_type(t.clone())).child(small(label, theme::TEXT_SECONDARY)));
    }
    page = page
        .child(filters)
        .child(small(format!("{} 筆 · {}", vm.total, vm.range_text.clone().unwrap_or_else(|| DASH.into())), theme::TEXT_SECONDARY));
    if let Some(n) = &vm.buffer_note {
        page = page.child(small(n.clone(), theme::TEXT_MUTED));
    }
    let mut list = div().id("log-list").flex().flex_col().gap_1().overflow_y_scroll().max_h(px(560.0));
    match &vm.timeline {
        Timeline::NothingSelected => list = list.child(text("未選擇任何事件類型", theme::TEXT_MUTED)),
        Timeline::Rows(rows) if rows.is_empty() => list = list.child(text("沒有資料", theme::TEXT_MUTED)),
        Timeline::Rows(rows) => {
            for r in rows {
                let mut head = div().flex().gap_2().child(small(r.time_text(), theme::TEXT_MUTED)).child(small(r.event_type.clone(), theme::ACCENT));
                for t in &r.tags {
                    head = head.child(small(format!("[{t}]"), theme::WARNING));
                }
                list = list.child(card().child(head).child(small(r.detail.clone(), theme::TEXT_SECONDARY).whitespace_normal()));
            }
        }
    }
    page = page.child(list);
    if vm.older.is_some() {
        page = page.child(div().id("log-older").cursor_pointer().on_click(on_older).child(small("載入更早事件 ↓", theme::ACCENT)));
    }
    page.child(small(vm.footer.clone(), theme::TEXT_MUTED))
}

// ---- scanner --------------------------------------------------------------------------------

/// The Funding Rate Matrix: a virtualized table (only visible rows are drawn).
pub struct ScannerTable {
    pub rows: Vec<ScanRow>,
    pub now_ms: i64,
    pub clocks: BTreeMap<Exchange, ClockState>,
    /// "加入交易單" cell per row (same order as `rows`).
    pub candidates: Vec<super::trading_pages::CandidateCell>,
    /// Clicked symbols, drained by the shell (the table cannot reach the shell directly).
    pub toggles: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
}

const SCAN_COLS: [(&str, &str, f32); 12] = [
    ("rank", "Rank", 50.0),
    ("symbol", "Symbol", 120.0),
    ("cov", "覆蓋", 50.0),
    ("cd", "結算倒數", 110.0),
    ("binance", "Binance", 150.0),
    ("bybit", "Bybit", 150.0),
    ("okx", "OKX", 150.0),
    ("dir", "最佳套利方向", 160.0),
    ("gross", "Gross Spread %", 120.0),
    ("net", "Net Edge %", 200.0),
    ("ok", "達標", 60.0),
    ("add", "加入交易單", 130.0),
];

fn rate_cell(c: &RateCell) -> Div {
    let mut d = div().flex().gap_1().items_center().child(small(c.text(), tone(c.tone())));
    if let Some(tag) = c.tag() {
        d = d.child(small(tag, theme::INFO));
    }
    for n in c.notes() {
        d = d.child(small(n, theme::TEXT_MUTED));
    }
    d
}

impl TableDelegate for ScannerTable {
    fn columns_count(&self, _: &App) -> usize {
        SCAN_COLS.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, ix: usize, _: &App) -> Column {
        Column::new(SCAN_COLS[ix].0, SCAN_COLS[ix].1).width(px(SCAN_COLS[ix].2))
    }
    fn render_td(&mut self, row: usize, col: usize, _: &mut Window, _: &mut Context<TableState<Self>>) -> impl IntoElement {
        let Some(r) = self.rows.get(row) else { return div() };
        match col {
            0 => small(format!("{:02}", r.rank), theme::TEXT_MUTED),
            1 => small(r.symbol.clone(), theme::TEXT_PRIMARY),
            2 => small(r.coverage_text(), theme::TEXT_SECONDARY),
            3 => {
                let clock = r.countdown_target.map(|(_, e)| self.clocks.get(&e).copied().unwrap_or(ClockState::Unsynced)).unwrap_or(ClockState::Unsynced);
                small(scanner::countdown(r.countdown_target, clock, self.now_ms).text(), theme::TEXT_SECONDARY)
            }
            4..=6 => rate_cell(&r.cells[col - 4].1),
            7 => small(r.direction_text(), theme::TEXT_SECONDARY),
            8 => {
                let mut d = div().flex().gap_1().child(small(r.gross_text(), theme::TEXT_SECONDARY));
                if r.intervals_differ {
                    d = d.child(small("週期不同", theme::WARNING));
                }
                d
            }
            9 => small(r.net_edge.text(), tone(r.net_edge.tone())),
            11 => {
                let cell = self.candidates.get(row).cloned().unwrap_or_default();
                let (mark, color) = match (&cell.blocked, cell.checked) {
                    (_, true) => ("☑ 已加入".to_string(), theme::ACCENT),
                    (None, false) => ("☐ 加入".to_string(), theme::TEXT_PRIMARY),
                    (Some(why), false) => (format!("☐ {why}"), theme::TEXT_MUTED),
                };
                let (toggles, symbol) = (self.toggles.clone(), r.symbol.clone());
                div().child(
                    div()
                        .id(("cand", row))
                        .cursor_pointer()
                        .on_click(move |_, _, _| toggles.borrow_mut().push(symbol.clone()))
                        .child(small(mark, color)),
                )
            }
            _ => small(r.qualified.text(), if r.qualified == scanner::Qualified::Yes { theme::POSITIVE } else { theme::TEXT_MUTED }),
        }
    }
}
