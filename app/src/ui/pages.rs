//! GPUI drawing of the read-only pages and the alert banner (ui-readonly-pages). Thin by design
//! (design D1): every number, label and decision comes from the view-models in `ui::<page>`;
//! this file only lays them out with theme tokens. Verified by screenshots (task 4.1).

use std::collections::BTreeMap;

use gpui_kit::component::chart::PieChart;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::*;
use tong_funding_core::types::{Decimal, Exchange};

use super::banner::{BadgeVm, BannerItem, BannerVm, DrawerVm, Freshness, LoadView};
use super::bridge::ClockState;
use super::dashboard::{self, ConnectedCard, DashboardVm, ExchangeCard, MarginDistribution};
use super::format::{self, DASH};
use super::positions::{PairCard, PositionsVm, TableView};
use super::scan_view::{ScanColumn, ScanEvent, SortDir, SortState};
use super::scanner::{self, RateCell, ScanRow};
use super::system_log::{SystemLogVm, Timeline};
use super::theme::{self, Tone};
use super::units::{fs, rx};

pub fn text(s: impl Into<SharedString>, color: u32) -> Div {
    div().text_color(rgb(color)).child(s.into())
}

pub fn small(s: impl Into<SharedString>, color: u32) -> Div {
    text(s, color).text_size(fs(10.0))
}

pub fn card() -> Div {
    div().p_3().rounded_md().bg(rgb(theme::BG_CARD)).border_1().border_color(rgb(theme::BORDER)).flex().flex_col().gap_1()
}

pub fn title(zh: &str, en: &str) -> Div {
    div()
        .flex()
        .items_end()
        .gap_3()
        .child(text(zh.to_string(), theme::TEXT_PRIMARY).text_size(fs(21.0)))
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
pub fn banner<F, G>(vm: &BannerVm, mut on_dismiss: F, mut on_link: G, on_open: &dyn Fn(String) -> ClickFn) -> Option<Div>
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
        for (j, l) in item.links.iter().enumerate() {
            row = row.child(open_button(SharedString::from(format!("banner-open-{i}-{j}")), &l.label, on_open(l.url.clone())));
        }
        if item.closable {
            row = row.child(div().id(("banner-close", i)).cursor_pointer().on_click(on_dismiss(i)).child(small("×", theme::TEXT_MUTED)));
        }
        col = col.child(row);
    }
    Some(col)
}

/// The header indicator of the notifications that are not in the banner (takes a few words of space).
pub fn badge(vm: &BadgeVm, open: bool, on_click: ClickFn) -> Stateful<Div> {
    div()
        .id("alert-badge")
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .bg(rgb(if open { theme::BG_BASE } else { theme::BG_CARD }))
        .border_1()
        .border_color(rgb(if vm.reviews > 0 { theme::WARNING } else { theme::BORDER }))
        .on_click(on_click)
        .child(small(format!("{} {}", vm.text, if open { "▴" } else { "▾" }), theme::WARNING))
}

fn drawer_row<F, G>(i: usize, item: &BannerItem, ack: &mut F, link: &mut G) -> Div
where
    F: FnMut(usize) -> ClickFn,
    G: FnMut(usize) -> ClickFn,
{
    let mut row = div()
        .flex()
        .items_start()
        .gap_2()
        .child(div().px_2().rounded_sm().bg(rgb(theme::BG_CARD)).child(small(item.category, theme::WARNING)))
        .child(small(item.message.clone(), theme::TEXT_PRIMARY).flex_1());
    if let Some(age) = &item.age_text {
        row = row.child(small(age.clone(), theme::TEXT_MUTED));
    }
    if let Some((label, _)) = item.link {
        row = row.child(div().id(("drawer-link", i)).cursor_pointer().on_click(link(i)).child(small(format!("{label} →"), theme::ACCENT)));
    }
    if item.closable {
        row = row.child(div().id(("drawer-ack", i)).px_2().rounded_sm().cursor_pointer().border_1().border_color(rgb(theme::BORDER)).on_click(ack(i)).child(small("已確認", theme::TEXT_SECONDARY)));
    }
    row
}

/// The floating list behind the badge: notices and data findings, each with "已確認". Item indexes
/// run over the notices first, then the findings. `ack_all(true)` = findings, `ack_all(false)` = notices.
pub fn drawer<F, G, H>(d: &DrawerVm, mut ack: F, mut link: G, mut ack_all: H, close: ClickFn) -> Div
where
    F: FnMut(usize) -> ClickFn,
    G: FnMut(usize) -> ClickFn,
    H: FnMut(bool) -> ClickFn,
{
    let mut col = div()
        .absolute()
        .top(rx(46.0))
        .right(rx(16.0))
        .w(rx(760.0))
        .max_h(rx(520.0))
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_md()
        .bg(rgb(theme::BG_SURFACE))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .occlude()
        .child(
            div()
                .flex()
                .justify_between()
                .child(small("通知", theme::TEXT_MUTED))
                .child(div().id("drawer-close").cursor_pointer().on_click(close).child(small("收合 ▴", theme::TEXT_MUTED))),
        );
    let n = d.notices.len();
    if !d.notices.is_empty() {
        col = col.child(
            div()
                .flex()
                .justify_between()
                .child(small(format!("注意 {n}"), theme::TEXT_SECONDARY))
                .child(div().id("drawer-ack-notices").cursor_pointer().on_click(ack_all(false)).child(small("全部確認", theme::TEXT_MUTED))),
        );
        for (i, item) in d.notices.iter().enumerate() {
            col = col.child(drawer_row(i, item, &mut ack, &mut link));
        }
    }
    if !d.reviews.is_empty() {
        col = col.child(
            div()
                .flex()
                .justify_between()
                .child(small(format!("資料問題 {}（已結案超過 3 天的不再列出）", d.reviews.len()), theme::TEXT_SECONDARY))
                .child(div().id("drawer-ack-reviews").cursor_pointer().on_click(ack_all(true)).child(small("全部確認", theme::TEXT_MUTED))),
        );
        for (i, item) in d.reviews.iter().enumerate() {
            col = col.child(drawer_row(n + i, item, &mut ack, &mut link));
        }
    }
    col
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
        .child(text(c.exchange.name(), theme::TEXT_PRIMARY).text_size(fs(14.0)))
        .child(small(c.status_label.clone(), theme::POSITIVE));
    let total = div()
        .flex()
        .gap_2()
        .items_end()
        .child(small("EXCHANGE TOTAL", theme::TEXT_MUTED))
        .child(text(format!("{} USDT", format::money(c.value, 2)), theme::TEXT_PRIMARY).text_size(fs(14.0)))
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
        div().flex().gap_2().children(["Asset", "Price", "Quantity", "Value (USDT)", "% of Exchange"].map(|h| small(h, theme::TEXT_MUTED).w(rx(110.0)))),
    );
    for a in &c.assets {
        table = table.child(div().flex().gap_2().children(
            [a.name.clone(), a.price_text(), a.quantity_text(), a.value_text(), a.pct_text()].map(|s| small(s, theme::TEXT_SECONDARY).w(rx(110.0))),
        ));
    }
    table = table
        .child(div().flex().gap_2().children(
            ["合計".to_string(), String::new(), String::new(), format::money(c.value, 2), "100.0000%".into()].map(|s| small(s, theme::TEXT_PRIMARY).w(rx(110.0))),
        ))
        .child(small(dashboard::EQUITY_FOOTNOTE, theme::TEXT_MUTED));
    let mut out = card().w(rx(560.0)).child(header).child(total).child(div().flex().gap_6().child(asset_chart).child(margin_chart)).child(table);
    if let Some(n) = &c.stale_note {
        out = out.child(small(n.clone(), theme::WARNING));
    }
    out
}

pub fn dashboard_page(vm: &DashboardVm) -> Div {
    let mut total = card()
        .child(small("Total Portfolio Value", theme::TEXT_MUTED))
        .child(text(format!("{} USDT", format::money(vm.total, 2)), theme::TEXT_PRIMARY).text_size(fs(21.0)))
        .child(small(vm.exchanges_text.clone(), theme::TEXT_SECONDARY));
    for n in [&vm.excluded_note, &vm.unvalued_note].into_iter().flatten() {
        total = total.child(small(n.clone(), theme::WARNING));
    }
    let mut top = div().flex().gap_3().child(total);
    for c in &vm.cards {
        if let ExchangeCard::Connected(c) = c {
            top = top.child(
                card()
                    .child(small(format!("{} Value", c.exchange.name()), theme::TEXT_MUTED))
                    .child(text(format::money(c.value, 2), theme::TEXT_PRIMARY).text_size(fs(14.0)))
                    .child(small(DashboardVm::pct_text(c), theme::TEXT_SECONDARY)),
            );
        }
    }
    top = top.child(
        card()
            .child(small("Open Positions", theme::TEXT_MUTED))
            .child(text(vm.open_positions.to_string(), theme::TEXT_PRIMARY).text_size(fs(14.0)))
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

/// A "前往交易所" button: the page opens the URL in the default browser.
fn open_button(id: SharedString, label: &str, on: ClickFn) -> Stateful<Div> {
    div().id(id).px_2().rounded_sm().cursor_pointer().border_1().border_color(rgb(theme::BORDER)).on_click(on).child(small(format!("{label} ↗"), theme::ACCENT))
}

fn pair_card(c: &PairCard, on_recheck: &dyn Fn(String) -> ClickFn, on_open: &dyn Fn(String) -> ClickFn, on_flatten: &dyn Fn(super::live_actions::FlattenButton) -> ClickFn) -> Div {
    let mut out = card().w(rx(360.0)).child(
        div()
            .flex()
            .justify_between()
            .child(text(format!("{} · 中性組合{}", c.symbol, c.badge.map_or(String::new(), |b| format!(" · {b}"))), if c.badge.is_some() { theme::NEGATIVE } else { theme::TEXT_PRIMARY }))
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
    // spread-position-tag: live tag (word + percentage + 標記價); absent when a price is missing / stale.
    if let Some(sp) = &c.spread {
        out = out.child(small(sp.text.clone(), tone(sp.tone)));
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
    // live-guardrails (Live only): the exchange's own page, and the flatten of a bare leg.
    if !c.links.is_empty() {
        let mut row = div().flex().flex_wrap().gap_2();
        for (j, l) in c.links.iter().enumerate() {
            row = row.child(open_button(SharedString::from(format!("open-{}-{j}", c.pair_id)), &l.label, on_open(l.url.clone())));
        }
        out = out.child(row);
    }
    if !c.flatten.is_empty() {
        out = out.child(small(super::live_actions::FLATTEN_TITLE, theme::WARNING));
        let mut row = div().flex().flex_wrap().gap_2();
        for b in &c.flatten {
            let id = SharedString::from(format!("flatten-{}-{}", c.pair_id, b.leg.as_str()));
            row = row.child(div().id(id).px_2().rounded_sm().cursor_pointer().border_1().border_color(rgb(theme::WARNING)).on_click(on_flatten(b.clone())).child(small(b.label.clone(), theme::WARNING)));
        }
        out = out.child(row);
    }
    // funding-slot-tracing: where each settlement's funding stands, and a re-query while unsettled.
    for line in &f.tracing {
        out = out.child(small(line.clone(), if line.contains("需人工檢查") { theme::WARNING } else { theme::TEXT_SECONDARY }));
    }
    if f.recheckable {
        out = out.child(
            div()
                .id(SharedString::from(format!("recheck-{}", c.pair_id)))
                .px_2()
                .rounded_sm()
                .cursor_pointer()
                .border_1()
                .border_color(rgb(theme::BORDER))
                .on_click(on_recheck(c.pair_id.clone()))
                .child(small("重新查詢資金費", theme::TEXT_SECONDARY)),
        );
    }
    out
}

pub type ClickFn = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

pub fn positions_page(
    vm: &PositionsVm,
    selected_ex: &dyn Fn(Exchange) -> bool,
    selected_coin: &dyn Fn(&str) -> bool,
    on_ex: &dyn Fn(Exchange) -> ClickFn,
    on_coin: &dyn Fn(String) -> ClickFn,
    on_recheck: &dyn Fn(String) -> ClickFn,
    on_open: &dyn Fn(String) -> ClickFn,
    on_flatten: &dyn Fn(super::live_actions::FlattenButton) -> ClickFn,
) -> Div {
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
    let mut table = div().flex().flex_col().gap_1().child(div().flex().gap_2().children(HEAD.map(|h| small(h, theme::TEXT_MUTED).w(rx(120.0)))));
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
                    line = line.child(small(c, color).w(rx(120.0)));
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
        .child(div().flex().flex_wrap().gap_3().children(vm.pair_cards.iter().map(|c| pair_card(c, on_recheck, on_open, on_flatten))))
}

// ---- system log -----------------------------------------------------------------------------

pub fn system_log_page(
    vm: Option<&SystemLogVm>,
    error: Option<&LoadView>,
    selected: &dyn Fn(&str) -> bool,
    on_type: &dyn Fn(String) -> ClickFn,
    on_older: ClickFn,
    identity: crate::identity::ProcessIdentity,
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
    let mut list = div().id("log-list").flex().flex_col().gap_1().overflow_y_scroll().max_h(rx(560.0));
    match &vm.timeline {
        Timeline::NothingSelected => list = list.child(text("未選擇任何事件類型", theme::TEXT_MUTED)),
        Timeline::Rows(rows) if rows.is_empty() => list = list.child(text("沒有資料", theme::TEXT_MUTED)),
        Timeline::Rows(rows) => {
            for r in rows {
                let mut head = div().flex().gap_2().child(small(r.time_text(), theme::TEXT_MUTED)).child(small(r.event_type.clone(), theme::ACCENT));
                if let Some(b) = super::system_log::live_badge(r, identity) {
                    head = head.child(small(b, theme::NEGATIVE));
                }
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
    /// The shown columns, in display order (`columns_count`/`column`/`render_td` index into this).
    pub visible: Vec<ScanColumn>,
    /// Current sort, only used to draw the header icon (the shell has already ordered `rows`).
    pub sort: Option<SortState>,
    /// Sort clicks, drained by the shell like `toggles`.
    pub events: std::rc::Rc<std::cell::RefCell<Vec<ScanEvent>>>,
}

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

impl ScannerTable {
    /// Rows start with the default order and every column shown.
    pub fn new(now_ms: i64, toggles: std::rc::Rc<std::cell::RefCell<Vec<String>>>, events: std::rc::Rc<std::cell::RefCell<Vec<ScanEvent>>>) -> Self {
        ScannerTable { rows: Vec::new(), now_ms, clocks: Default::default(), candidates: Vec::new(), toggles, visible: ScanColumn::ALL.to_vec(), sort: None, events }
    }
}

impl TableDelegate for ScannerTable {
    fn columns_count(&self, _: &App) -> usize {
        self.visible.len()
    }
    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }
    fn column(&self, ix: usize, _: &App) -> Column {
        let c = self.visible.get(ix).copied().unwrap_or(ScanColumn::Symbol);
        // No dragging or resizing: the delegate maps shown columns itself, so the table must not reorder them.
        let mut col = Column::new(c.key(), c.title()).width(px(c.width())).resizable(false).movable(false);
        if c.sortable() {
            col = match self.sort {
                Some(s) if s.column == c && s.dir == SortDir::Asc => col.ascending(),
                Some(s) if s.column == c => col.descending(),
                _ => col.sortable(),
            };
        }
        if c.fixed_left() {
            col = col.fixed_left();
        }
        col
    }
    /// The header title is clickable too (the library only reacts to its small sort icon).
    fn render_th(&mut self, col_ix: usize, _: &mut Window, _: &mut Context<TableState<Self>>) -> impl IntoElement {
        let c = self.visible.get(col_ix).copied().unwrap_or(ScanColumn::Symbol);
        let mut title = div().id(("scan-th", col_ix)).test_support().size_full().child(c.title());
        if c.sortable() {
            let events = self.events.clone();
            title = title.cursor_pointer().on_click(move |_, _, _| events.borrow_mut().push(ScanEvent::CycleSort(c)));
        }
        title
    }
    /// The library's sort icon was clicked; it already cycled, so record the result it reports.
    fn perform_sort(&mut self, col_ix: usize, sort: ColumnSort, _: &mut Window, _: &mut Context<TableState<Self>>) {
        let Some(c) = self.visible.get(col_ix).copied() else { return };
        let dir = match sort {
            ColumnSort::Ascending => Some(SortDir::Asc),
            ColumnSort::Descending => Some(SortDir::Desc),
            ColumnSort::Default => None,
        };
        self.events.borrow_mut().push(ScanEvent::SetSort(c, dir));
    }
    fn render_td(&mut self, row: usize, col: usize, _: &mut Window, _: &mut Context<TableState<Self>>) -> impl IntoElement {
        let (Some(r), Some(c)) = (self.rows.get(row), self.visible.get(col).copied()) else { return div() };
        match c {
            ScanColumn::Rank => small(format!("{:02}", r.rank), theme::TEXT_MUTED),
            ScanColumn::Symbol => small(r.symbol.clone(), theme::TEXT_PRIMARY),
            ScanColumn::Coverage => small(r.coverage_text(), theme::TEXT_SECONDARY),
            ScanColumn::Countdown => {
                let clock = r.countdown_target.map(|(_, e)| self.clocks.get(&e).copied().unwrap_or(ClockState::Unsynced)).unwrap_or(ClockState::Unsynced);
                small(scanner::countdown(r.countdown_target, clock, self.now_ms).text(), theme::TEXT_SECONDARY)
            }
            ScanColumn::Binance => rate_cell(&r.cells[0].1),
            ScanColumn::Bybit => rate_cell(&r.cells[1].1),
            ScanColumn::Okx => rate_cell(&r.cells[2].1),
            ScanColumn::Direction => small(r.direction_text(), theme::TEXT_SECONDARY),
            ScanColumn::Gross => {
                let mut d = div().flex().gap_1().child(small(r.gross_text(), theme::TEXT_SECONDARY));
                if r.intervals_differ {
                    d = d.child(small("週期不同", theme::WARNING));
                }
                if let (Some(label), Some(hint)) = (r.edge_kind_label(), r.edge_kind_hint()) {
                    // The id follows the symbol, not the row index: gpui keeps tooltip state per element id, so an
                    // index would show the previous row's text on whatever symbol a re-sort puts there.
                    d = d.child(
                        div()
                            .id(SharedString::from(format!("edge-kind-{}", r.symbol)))
                            .test_support()
                            .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
                            .child(small(label, theme::TEXT_SECONDARY)),
                    );
                }
                d
            }
            ScanColumn::NetEdge => small(r.net_edge.text(), tone(r.net_edge.tone())),
            ScanColumn::Add => {
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
                        .test_support()
                        .cursor_pointer()
                        .on_click(move |_, _, _| toggles.borrow_mut().push(symbol.clone()))
                        .child(small(mark, color)),
                )
            }
            ScanColumn::Qualified => small(r.qualified.text(), if r.qualified == scanner::Qualified::Yes { theme::POSITIVE } else { theme::TEXT_MUTED }),
        }
    }
}

#[cfg(test)]
mod spread_tag_smoke {
    // not `use super::*`: its gpui_kit glob would shadow the std `#[test]` attribute
    use super::pair_card;
    use crate::ui::bridge::{MarketFeed, UiSnapshot};
    use crate::ui::positions::{build, Filter};
    use crate::ui::testkit::{d, loaded, obs, pair, position};
    use tong_funding_core::pair::PairState;
    use tong_funding_core::types::Exchange::{Binance, Bybit};

    /// The positions page builds its pair card (with the spread-tag line) from a live snapshot
    /// without panicking; text/tone content is asserted in the view-model tests (`positions_tests`).
    /// No window is opened (`Div` cannot be inspected without one).
    #[test]
    fn a_pair_card_with_a_spread_tag_draws() {
        let now = 1_791_201_600_000;
        let mut s = UiSnapshot::default();
        s.accounts.insert(Binance, loaded(vec![], vec![position(Binance, "BTCUSDT", "0.02", "60000", "60200", "3", "4", Some("400"))], now));
        s.accounts.insert(Bybit, loaded(vec![], vec![position(Bybit, "BTCUSDT", "-0.02", "60000", "60200", "3", "-4", Some("400"))], now));
        s.pairs = vec![pair("p1", "BTCUSDT", Binance, Bybit, PairState::Reconciled)];
        for (ex, px) in [(Binance, "100"), (Bybit, "100.2")] {
            let mut o = obs(ex, "BTCUSDT", "0.0001", 28_800, now + 3_600_000, now - 500);
            o.mark_price = d(px);
            s.market.insert(ex, MarketFeed { observations: vec![o], last_success_at: Some(now - 500), last_error: None });
        }
        let vm = build(&s, &Filter::default(), now);
        let card = &vm.pair_cards[0];
        assert!(card.spread.is_some());
        let _ = pair_card(card, &|_| Box::new(|_, _, _| {}), &|_| Box::new(|_, _, _| {}), &|_| Box::new(|_, _, _| {}));
    }
}
