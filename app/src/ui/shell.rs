//! Main window shell: header (dual clock), alert banner, sidebar, routed content, status bar.
//! All decisions (order, labels, defaults, clock math, page contents, alerts) live in the tested
//! modules (`nav`, `clock`, `status`, and the `ui::<page>` view-models); this file only wires the
//! data source to them and lays them out.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::*;
use tong_funding_core::types::Exchange;

use super::alerts::{self, Alert, FreshStatus};
use super::banner::{self, AlertKey, LoadView, Region};
use super::bridge::{Bridge, KillSwitchState, ReadOnlyDataSource, SourceId, UiSnapshot};
use super::clock::{now_unix_secs, read_clock};
use super::dashboard::{self, DashboardVm};
use super::fonts::app_font;
use super::format;
use super::nav::{Page, DEBUG_WARNING};
use super::pages::{self, ClickFn, ScannerTable, small, text};
use super::positions::{self, PositionsVm};
use super::scanner::{self, RowsView, ScannerVm, TableState as ScanState};
use super::status::{Connection, StatusModel, ENVIRONMENT_LABEL};
use super::system_log::{self, LogFilter, LogRow, SystemLogVm};
use super::theme;
use crate::ports::{Clock, SystemClock};
use crate::store::event_query::EventQuery;

/// UI loop period: drains data updates; recomputes are rate-limited by the bridge (2 Hz).
const UI_TICK_MS: u64 = 100;

pub struct Shell {
    page: Page,
    now_secs: i64,
    now_ms: i64,
    status: StatusModel,
    source: Arc<dyn ReadOnlyDataSource>,
    bridge: Bridge,
    snap: UiSnapshot,
    scanner: ScannerVm,
    dashboard: DashboardVm,
    positions: PositionsVm,
    pos_filter: positions::Filter,
    alerts: Vec<Alert>,
    dismissed: BTreeSet<AlertKey>,
    only_qualified: bool,
    table: Entity<TableState<ScannerTable>>,
    log_filter: LogFilter,
    log_vm: Option<SystemLogVm>,
    log_error: Option<LoadView>,
    frames: Option<FrameRecorder>,
}

/// `TONG_FUNDING_FRAME_STATS=<seconds>`: measures frame intervals on the scanner page with the
/// real data feeds (ui-readonly-pages task 4.2) and prints one line per window, like the
/// `--bench-table` spike. Off unless the variable is set.
pub const FRAME_STATS_ENV: &str = "TONG_FUNDING_FRAME_STATS";

struct FrameRecorder {
    window: Duration,
    started: std::time::Instant,
    last: Option<std::time::Instant>,
    intervals_ms: Vec<f64>,
}

impl FrameRecorder {
    fn from_env() -> Option<Self> {
        let secs: u64 = std::env::var(FRAME_STATS_ENV).ok()?.parse().ok().filter(|s| *s > 0)?;
        Some(FrameRecorder { window: Duration::from_secs(secs), started: std::time::Instant::now(), last: None, intervals_ms: Vec::new() })
    }

    fn frame(&mut self, rows: usize) {
        let now = std::time::Instant::now();
        if let Some(prev) = self.last {
            self.intervals_ms.push(now.duration_since(prev).as_secs_f64() * 1000.0);
        }
        self.last = Some(now);
        if now.duration_since(self.started) < self.window {
            return;
        }
        let samples = &self.intervals_ms[self.intervals_ms.len().min(30)..];
        let running = super::frame_stats::split_paused(samples);
        match super::frame_stats::summarize(&running) {
            Some(s) => {
                let d = super::frame_stats::drops(samples, s.p50_ms);
                println!(
                    "FRAMES page=scanner rows={rows} window_s={} frames={} p50_ms={:.2} p95_ms={:.2} max_ms={:.2} dropped={} paused={}",
                    self.window.as_secs(),
                    s.frames,
                    s.p50_ms,
                    s.p95_ms,
                    s.max_ms,
                    d.dropped,
                    d.paused
                );
            }
            None => println!("FRAMES no frames recorded (stay on the scanner page with the window visible)"),
        }
        self.started = now;
        self.intervals_ms.clear();
    }
}

fn wall_ms() -> i64 {
    // The UI is the one place allowed to read the wall clock (engine and view-models get it injected).
    SystemClock.now_ms()
}

impl Shell {
    pub fn new(source: Arc<dyn ReadOnlyDataSource>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let now_ms = wall_ms();
        let snap = UiSnapshot::default();
        let table = cx.new(|cx| TableState::new(ScannerTable { rows: Vec::new(), now_ms, clocks: Default::default() }, window, cx));
        let mut shell = Shell {
            page: Page::default_page(),
            now_secs: now_unix_secs(),
            now_ms,
            status: StatusModel::default(),
            source,
            bridge: Bridge::default(),
            scanner: scanner::build(&snap, now_ms),
            dashboard: dashboard::build(&snap, now_ms),
            positions: positions::build(&snap, &positions::Filter::default()),
            alerts: alerts::alerts(&snap, now_ms),
            snap,
            pos_filter: positions::Filter::default(),
            dismissed: BTreeSet::new(),
            only_qualified: false,
            table,
            log_filter: LogFilter::default(),
            log_vm: None,
            log_error: None,
            frames: FrameRecorder::from_env(),
        };
        shell.recompute(cx);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(UI_TICK_MS)).await;
                if this.update(cx, |s, cx| s.tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        shell
    }

    /// Every 100 ms: merge data updates; recompute when the bridge allows; repaint clocks and
    /// countdowns once per second without recomputing the tables.
    fn tick(&mut self, cx: &mut Context<Self>) {
        self.now_ms = wall_ms();
        for u in self.source.drain_updates() {
            self.bridge.push(u);
        }
        let mut repaint = false;
        if let Some(snap) = self.bridge.take_if_due(self.now_ms) {
            self.snap = snap;
            self.recompute(cx);
            repaint = true;
        }
        let secs = now_unix_secs();
        if secs != self.now_secs {
            self.now_secs = secs;
            self.alerts = alerts::alerts(&self.snap, self.now_ms);
            banner::prune_dismissed(&mut self.dismissed, &self.alerts);
            let now = self.now_ms;
            self.table.update(cx, |t, cx| {
                t.delegate_mut().now_ms = now;
                cx.notify();
            });
            repaint = true;
        }
        if repaint {
            cx.notify();
        }
    }

    fn recompute(&mut self, cx: &mut Context<Self>) {
        self.scanner = scanner::build(&self.snap, self.now_ms);
        self.dashboard = dashboard::build(&self.snap, self.now_ms);
        self.positions = positions::build(&self.snap, &self.pos_filter);
        self.alerts = alerts::alerts(&self.snap, self.now_ms);
        banner::prune_dismissed(&mut self.dismissed, &self.alerts);
        self.status = self.status_model();
        self.sync_table(cx);
    }

    fn sync_table(&mut self, cx: &mut Context<Self>) {
        let rows: Vec<_> = match self.scanner.visible(self.only_qualified) {
            RowsView::Rows(r) => r.into_iter().cloned().collect(),
            RowsView::NoneQualified => Vec::new(),
        };
        let (now, clocks) = (self.now_ms, self.snap.clocks.clone());
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            d.rows = rows;
            d.now_ms = now;
            d.clocks = clocks;
            t.refresh(cx);
            cx.notify();
        });
    }

    fn status_model(&self) -> StatusModel {
        let mut s = StatusModel::default();
        for (name, conn) in &mut s.exchanges {
            let ex = Exchange::ALL.into_iter().find(|e| e.name() == *name).unwrap_or(Exchange::Okx);
            let online = self.snap.health_of(SourceId::MarketPoll(ex)).is_some_and(|h| alerts::source_status(h, self.now_ms) == FreshStatus::Online);
            *conn = if online { Connection::Connected } else { Connection::Disconnected };
        }
        s.kill_switch = match self.snap.system.kill_switch {
            KillSwitchState::Off if self.snap.system.store_halt.is_none() => super::status::KillSwitch::Disabled,
            _ => super::status::KillSwitch::Halted,
        };
        s
    }

    // ---- system log loading ---------------------------------------------------------------

    fn load_log(&mut self, older: Option<EventQuery>) {
        let query = older.clone().unwrap_or_else(|| system_log::first_query(&self.log_filter));
        let loaded: Vec<LogRow> = match (&older, &self.log_vm) {
            (Some(_), Some(vm)) => match &vm.timeline {
                system_log::Timeline::Rows(r) => r.clone(),
                system_log::Timeline::NothingSelected => Vec::new(),
            },
            _ => Vec::new(),
        };
        match self.source.load_events(&query) {
            Ok(page) => {
                let capacity = self.snap.scan_capacity;
                self.log_vm = Some(system_log::build(&page, &self.snap.scan_runs, capacity, &self.log_filter, &query, &loaded));
                self.log_error = None;
            }
            Err(e) => {
                let now = self.now_ms;
                self.log_error = Some(LoadView::Failed { error: e, at_ms: now, now_ms: now });
            }
        }
    }

    fn go(&mut self, page: Page) {
        self.page = page;
        if page == Page::SystemLogs {
            self.load_log(None);
        }
    }

    // ---- layout ---------------------------------------------------------------------------

    fn header(&self) -> Div {
        let c = read_clock(self.now_secs);
        let clock = |label: &str, date: String, time: String| {
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(div().text_color(rgb(theme::TEXT_MUTED)).child(label.to_string()))
                .child(div().text_color(rgb(theme::TEXT_SECONDARY)).child(date))
                .child(div().text_color(rgb(theme::TEXT_PRIMARY)).child(time))
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(44.0))
            .px_4()
            .bg(rgb(theme::BG_BASE))
            .border_b_1()
            .border_color(rgb(theme::BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().text_color(rgb(theme::ACCENT)).text_size(px(14.0)).child("◈ Funding Monitor"))
                    .child(div().px_2().py_1().rounded_sm().bg(rgb(theme::BG_CARD)).text_color(rgb(theme::WARNING)).child(ENVIRONMENT_LABEL)),
            )
            .child(div().flex().gap_6().child(clock("UTC", c.utc_date, c.utc_time)).child(clock("TAIPEI · UTC+8", c.taipei_date, c.taipei_time)))
    }

    fn banner(&self, cx: &mut Context<Self>) -> Option<Div> {
        let vm = banner::banner(&self.alerts, &self.dismissed);
        let keys: Vec<AlertKey> = vm.items.iter().map(|i| i.key.clone()).collect();
        let links: Vec<Option<Page>> = vm.items.iter().map(|i| i.link.map(|l| l.1)).collect();
        pages::banner(
            &vm,
            |i| {
                let key = keys[i].clone();
                Box::new(cx.listener(move |this: &mut Shell, _, _, cx| {
                    this.dismissed.insert(key.clone());
                    cx.notify();
                }))
            },
            |i| {
                let target = links[i].unwrap_or(Page::Positions);
                Box::new(cx.listener(move |this: &mut Shell, _, _, cx| {
                    this.go(target);
                    cx.notify();
                }))
            },
        )
    }

    fn nav_item(&self, idx: usize, page: Page, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.page == page;
        div()
            .id(("nav", idx))
            .flex()
            .flex_col()
            .px_3()
            .py_2()
            .cursor_pointer()
            .border_l_2()
            .border_color(rgb(if selected { theme::ACCENT } else { theme::BG_BASE }))
            .bg(rgb(if selected { theme::BG_CARD } else { theme::BG_BASE }))
            .hover(|s| s.bg(rgb(theme::BG_SURFACE)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.go(page);
                cx.notify();
            }))
            .child(div().text_size(px(13.0)).text_color(rgb(if selected { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY })).child(page.zh()))
            .child(div().text_size(px(10.0)).text_color(rgb(theme::TEXT_MUTED)).child(page.en()))
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        let mut bar = div().flex().flex_col().w(px(200.0)).flex_none().bg(rgb(theme::BG_BASE)).border_r_1().border_color(rgb(theme::BORDER));
        for (idx, page) in Page::ALL.into_iter().enumerate() {
            if page.is_debug() {
                bar = bar
                    .child(div().h(px(1.0)).mx_3().my_2().bg(rgb(theme::BORDER)))
                    .child(div().px_3().pb_1().text_size(px(10.0)).text_color(rgb(theme::WARNING)).child(format!("⚠ {DEBUG_WARNING}")));
            }
            bar = bar.child(self.nav_item(idx, page, cx));
        }
        bar
    }

    fn scanner_page(&self, cx: &mut Context<Self>) -> Div {
        let vm = &self.scanner;
        let refreshing = self.source.refresh_in_progress();
        let mut sources = div().flex().gap_4();
        for s in scanner::HEADER_SOURCES {
            let mut f = banner::freshness(&self.snap, s, self.now_ms);
            let label = s.label();
            let mut chip = pages::freshness_chip(&label, &f);
            if s == SourceId::BinanceWs {
                // The WebSocket header uses ONLINE / RECONNECTING / OFFLINE.
                let word = match f.status {
                    FreshStatus::Online => "ONLINE",
                    FreshStatus::Stale | FreshStatus::RateLimited => "RECONNECTING",
                    FreshStatus::Offline | FreshStatus::Unknown => "OFFLINE",
                };
                f.next_poll_s = None;
                chip = div().flex().gap_1().child(pages::freshness_chip(&label, &f)).child(small(word, theme::TEXT_SECONDARY));
            }
            sources = sources.child(chip);
        }
        let header = div()
            .flex()
            .justify_between()
            .items_center()
            .child(pages::title("掃幣", "Full-Market Scanner"))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .items_center()
                    .child(small(vm.threshold_text.clone(), theme::TEXT_SECONDARY))
                    .child(
                        div()
                            .id("to-risk")
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.go(Page::RiskSettings);
                                cx.notify();
                            }))
                            .child(small("於風控設定修改 →", theme::ACCENT)),
                    ),
            );
        let summary = |label: &str, value: String| pages::card().child(small(label.to_string(), theme::TEXT_MUTED)).child(text(value, theme::TEXT_PRIMARY).text_size(px(14.0)));
        let cards = div()
            .flex()
            .gap_3()
            .child(summary("掃描標的", vm.summary.scanned.to_string()))
            .child(summary("多所覆蓋", vm.summary.multi_coverage.to_string()))
            .child(summary("達標 · Net Edge ≥ 門檻", vm.summary.qualified.map_or_else(|| "未設定".to_string(), |n| n.to_string())));
        let toggle_color = if vm.decidable { theme::TEXT_PRIMARY } else { theme::TEXT_MUTED };
        let toggle = div()
            .id("only-qualified")
            .flex()
            .gap_2()
            .items_center()
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| {
                if this.scanner.decidable {
                    this.only_qualified = !this.only_qualified;
                    this.sync_table(cx);
                    cx.notify();
                }
            }))
            .child(small(if self.only_qualified && vm.decidable { "● 只顯示達標" } else { "○ 只顯示達標" }, toggle_color))
            .child(small(vm.match_text(), theme::TEXT_SECONDARY));
        let refresh = div()
            .id("refresh-now")
            .px_3()
            .py_1()
            .rounded_sm()
            .bg(rgb(if refreshing { theme::BG_SURFACE } else { theme::BG_CARD }))
            .border_1()
            .border_color(rgb(theme::BORDER))
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| {
                // Ignored while one is running (the data source refuses it too).
                let _ = this.source.request_refresh();
                cx.notify();
            }))
            .child(small(if refreshing { "刷新中…" } else { "立即刷新" }, if refreshing { theme::TEXT_MUTED } else { theme::ACCENT }));
        let controls = div()
            .flex()
            .justify_between()
            .items_center()
            .child(toggle)
            .child(div().flex().gap_3().items_center().child(sources).child(small(format!("最新掃描 {} UTC", format::utc_hms(vm.computed_at)), theme::TEXT_MUTED)).child(refresh));

        let mut body = div().flex().flex_col().gap_2();
        for (e, msg) in &vm.source_errors {
            body = body.child(small(format!("{} 行情更新失敗（顯示的是舊資料）：{msg}", e.name()), theme::WARNING));
        }
        match &vm.state {
            ScanState::Loading => body = body.child(text("載入中", theme::TEXT_MUTED)),
            ScanState::Unavailable { .. } => {
                let age = vm.oldest_data_age_ms.map(|a| format!("（資料 {} 秒前）", format::secs(a))).unwrap_or_default();
                body = body.child(text(format!("無法取得行情{age}"), theme::NEGATIVE));
            }
            ScanState::Ready => {}
        }
        if matches!(vm.visible(self.only_qualified), RowsView::NoneQualified) {
            body = body.child(text("目前沒有達標標的", theme::TEXT_MUTED));
        } else if !matches!(vm.state, ScanState::Loading) {
            body = body.child(div().h(px(560.0)).child(DataTable::new(&self.table).stripe(true).bordered(true)));
        }
        div().flex().flex_col().gap_3().child(header).child(cards).child(controls).child(body)
    }

    fn positions_page(&self, cx: &mut Context<Self>) -> Div {
        let f = self.pos_filter.clone();
        let ex_opts = self.positions.exchange_options.clone();
        let coin_opts = self.positions.coin_options.clone();
        let sel_ex = |e: Exchange| f.exchanges.as_ref().is_none_or(|s| s.contains(&e));
        let sel_coin = |c: &str| f.coins.as_ref().is_none_or(|s| s.contains(c));
        let on_ex = |e: Exchange| -> ClickFn {
            let opts = ex_opts.clone();
            Box::new(cx.listener(move |this: &mut Shell, _, _, cx| {
                let mut set = this.pos_filter.exchanges.clone().unwrap_or_else(|| opts.iter().copied().collect());
                if !set.remove(&e) {
                    set.insert(e);
                }
                this.pos_filter.exchanges = Some(set);
                this.positions = positions::build(&this.snap, &this.pos_filter);
                cx.notify();
            }))
        };
        let on_coin = |c: String| -> ClickFn {
            let opts = coin_opts.clone();
            Box::new(cx.listener(move |this: &mut Shell, _, _, cx| {
                let mut set = this.pos_filter.coins.clone().unwrap_or_else(|| opts.iter().cloned().collect());
                if !set.remove(&c) {
                    set.insert(c.clone());
                }
                this.pos_filter.coins = Some(set);
                this.positions = positions::build(&this.snap, &this.pos_filter);
                cx.notify();
            }))
        };
        pages::positions_page(&self.positions, &sel_ex, &sel_coin, &on_ex, &on_coin)
    }

    fn system_log_page(&self, cx: &mut Context<Self>) -> Div {
        let options: Vec<String> = self.log_vm.as_ref().map(|v| v.type_options.clone()).unwrap_or_default();
        let filter = self.log_filter.clone();
        let selected = |t: &str| filter.types.as_ref().is_none_or(|s| s.contains(t));
        let on_type = |t: String| -> ClickFn {
            let opts = options.clone();
            Box::new(cx.listener(move |this: &mut Shell, _, _, cx| {
                let mut set: BTreeSet<String> = this.log_filter.types.clone().unwrap_or_else(|| opts.iter().cloned().collect());
                if !set.remove(&t) {
                    set.insert(t.clone());
                }
                this.log_filter.types = Some(set);
                this.load_log(None);
                cx.notify();
            }))
        };
        let on_older: ClickFn = Box::new(cx.listener(|this: &mut Shell, _, _, cx| {
            let older = this.log_vm.as_ref().and_then(|v| v.older.clone());
            if older.is_some() {
                this.load_log(older);
            }
            cx.notify();
        }));
        pages::system_log_page(self.log_vm.as_ref(), self.log_error.as_ref(), &selected, &on_type, on_older)
    }

    fn content(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let inner = match self.page {
            Page::Overview => pages::dashboard_page(&self.dashboard),
            Page::Scanner => self.scanner_page(cx),
            Page::Positions => self.positions_page(cx),
            Page::SystemLogs => self.system_log_page(cx),
            other => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(pages::title(other.zh(), other.en()))
                .child(div().mt_4().text_color(rgb(theme::TEXT_MUTED)).child("（尚未實作）")),
        };
        div().id("content").flex_1().p_6().overflow_y_scroll().child(inner)
    }

    fn status_bar(&self) -> Div {
        let bar = div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(28.0))
            .px_4()
            .bg(rgb(theme::BG_BASE))
            .border_t_1()
            .border_color(rgb(theme::BORDER))
            .text_size(px(10.0));
        let mut left = div().flex().items_center().gap_4().child(div().px_2().rounded_sm().bg(rgb(theme::BG_CARD)).text_color(rgb(theme::ACCENT)).child(self.status.mode.label()));
        for (name, conn) in &self.status.exchanges {
            let dot = if *conn == Connection::Connected { theme::POSITIVE } else { theme::TEXT_MUTED };
            left = left.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().size(px(6.0)).rounded_full().bg(rgb(dot)))
                    .child(div().text_color(rgb(theme::TEXT_SECONDARY)).child(*name))
                    .child(div().text_color(rgb(theme::TEXT_MUTED)).child(conn.label())),
            );
        }
        bar.child(left).child(
            div()
                .flex()
                .gap_1()
                .child(div().text_color(rgb(theme::TEXT_MUTED)).child("Kill switch"))
                .child(div().text_color(rgb(theme::TEXT_SECONDARY)).child(self.status.kill_switch.label())),
        )
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(rec) = &mut self.frames {
            if self.page == Page::Scanner {
                rec.frame(self.scanner.rows.len());
                // Worst case on purpose (like `--bench-table`): redraw every frame while measuring.
                window.request_animation_frame();
            } else {
                rec.last = None;
            }
        }
        let mut root = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::BG_DEEPEST))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .text_size(px(theme::FONT_SIZE_BODY))
            .font(app_font(FontWeight::NORMAL));
        // Region order comes from the tested `banner::layout`: the banner is on every page.
        for region in banner::layout(self.page) {
            root = match region {
                Region::Header => root.child(self.header()),
                Region::Banner => match self.banner(cx) {
                    Some(b) => root.child(b),
                    None => root,
                },
                Region::Content => root.child(div().flex().flex_1().min_h_0().child(self.sidebar(cx)).child(self.content(cx))),
                Region::StatusBar => root.child(self.status_bar()),
            };
        }
        root
    }
}
