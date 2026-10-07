//! GPUI drawing of the four trading pages and the scanner's Candidate List (ui-trading-pages).
//! Thin by design (design D1): every number, label, confirmation and disabled reason comes from
//! the view-models (`staged_orders`, `contract_settings`, `risk_settings`, `manual_order`,
//! `candidates`); this file holds the text inputs, lays the view-models out with theme tokens and
//! forwards clicks as engine commands through the shell's `CommandSink`. Confirmations are inline
//! panels (a GPUI dialog component was not verified, design Risks). Verified by screenshots (5.1).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::*;
use tong_funding_core::risk::ExecutionMode;
use tong_funding_core::types::Exchange;

use super::bridge::{Settings, SourceUpdate, UiSnapshot};
use super::candidates::{self, CandidateList};
use super::contract_settings::{self, CalcMode, ContractForm};
use super::format::{self, DASH};
use super::manual_order::{self, CancelForm, CancelPrefill, ManualConfirm, ManualForm, ManualPrefill, PickState};
use super::nav::{Page, DEBUG_WARNING};
use super::pages::{card, small, text, title, ClickFn};
use super::risk_settings::{self, Field, RiskForm, Strictness, GLOBAL_FIELDS, MODE_OPTIONS, OVERRIDE_FIELDS};
use super::scanner::ScanRow;
use super::shell::Shell;
use super::staged_orders::{self, CloseConfirm, CostView, PendingConfirm, RunningRow};
use super::symbol_options;
use super::symbol_select::{CoinPicker, SymbolPicker};
use super::theme;
use super::units::rx;
use crate::engine::command::{Command, CommandReply};
use crate::engine::ports::{AccountPosition, OrderSide};

/// The "加入交易單" cell of one scanner row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CandidateCell {
    pub checked: bool,
    /// Why the row cannot be ticked (`None` = it can).
    pub blocked: Option<String>,
}

type In = Entity<InputState>;

/// Inputs, selections and pending confirmations of the trading pages.
pub struct TradingState {
    pub toggles: Rc<RefCell<Vec<String>>>,
    pub candidates: CandidateList,
    pub settings_seen: bool,
    pub staged_sel: BTreeSet<String>,
    pub staged_confirm: Option<PendingConfirm>,
    pub staged_note: Option<String>,
    pub close_confirm: Option<CloseConfirm>,
    pub close_now_confirm: Option<RunningRow>,
    c_notional: In,
    c_leverage: In,
    c_margin: In,
    c_symbol: SymbolPicker,
    pub c_mode: CalcMode,
    contract_loaded: bool,
    contract_reload: bool,
    c_requested: String,
    r_fields: BTreeMap<Field, In>,
    r_over: BTreeMap<(Exchange, Field), In>,
    pub r_over_on: BTreeSet<(Exchange, Field)>,
    pub r_allowed: BTreeSet<Exchange>,
    r_coins: CoinPicker,
    risk_loaded: bool,
    risk_reload: bool,
    pub mode_confirm: Option<ExecutionMode>,
    pub mode_error: Option<String>,
    m_symbol: SymbolPicker,
    m_qty: In,
    /// Leverage of a manual opening order; filled once from the contract settings, then editable.
    m_lev: In,
    m_lev_loaded: bool,
    x_symbol: SymbolPicker,
    x_id: In,
    pub m_exchange: Exchange,
    pub m_side: OrderSide,
    pub m_reduce: bool,
    pub x_exchange: Exchange,
    m_requested: String,
    /// A picker click's values, written into the inputs on the next render (needs a `Window`).
    pending_manual: Option<ManualPrefill>,
    pending_cancel: Option<CancelPrefill>,
    pub manual_confirm: Option<ManualConfirm>,
}

fn new_input(window: &mut Window, cx: &mut Context<Shell>, value: &str) -> In {
    let v = value.to_string();
    cx.new(|cx| InputState::new(window, cx).default_value(v))
}

fn val(i: &In, cx: &App) -> String {
    i.read(cx).value().to_string()
}

fn set_val(i: &In, v: String, window: &mut Window, cx: &mut Context<Shell>) {
    i.update(cx, |s, cx| s.set_value(v, window, cx));
}

impl TradingState {
    pub fn new(window: &mut Window, cx: &mut Context<Shell>) -> TradingState {
        let r_fields = GLOBAL_FIELDS.iter().map(|f| (*f, new_input(window, cx, ""))).collect();
        let mut r_over = BTreeMap::new();
        for ex in Exchange::ALL {
            for f in OVERRIDE_FIELDS {
                r_over.insert((ex, f), new_input(window, cx, ""));
            }
        }
        TradingState {
            toggles: Rc::new(RefCell::new(Vec::new())),
            candidates: CandidateList::default(),
            settings_seen: false,
            staged_sel: BTreeSet::new(),
            staged_confirm: None,
            staged_note: None,
            close_confirm: None,
            close_now_confirm: None,
            c_notional: new_input(window, cx, "1000"),
            c_leverage: new_input(window, cx, "5"),
            c_margin: new_input(window, cx, "200"),
            c_symbol: SymbolPicker::new("BTCUSDT", window, cx),
            c_mode: CalcMode::LeverageToMargin,
            contract_loaded: false,
            contract_reload: false,
            c_requested: String::new(),
            r_fields,
            r_over,
            r_over_on: BTreeSet::new(),
            r_allowed: Exchange::ALL.into_iter().collect(),
            r_coins: CoinPicker::new(window, cx),
            risk_loaded: false,
            risk_reload: false,
            mode_confirm: None,
            mode_error: None,
            m_symbol: SymbolPicker::new("BTCUSDT", window, cx),
            m_qty: new_input(window, cx, ""),
            m_lev: new_input(window, cx, "5"),
            m_lev_loaded: false,
            x_symbol: SymbolPicker::new("BTCUSDT", window, cx),
            x_id: new_input(window, cx, ""),
            m_exchange: Exchange::Binance,
            m_side: OrderSide::Buy,
            m_reduce: false,
            x_exchange: Exchange::Binance,
            m_requested: String::new(),
            pending_manual: None,
            pending_cancel: None,
            manual_confirm: None,
        }
    }

    /// After a successful save, the forms reload from the stored values (read back from the store).
    pub fn observe(&mut self, u: &SourceUpdate) {
        match u {
            SourceUpdate::CommandResult(o) if o.reply == CommandReply::Accepted => {
                if o.label == "儲存風控設定" {
                    self.risk_reload = true;
                }
                if o.label == "儲存合約模板" {
                    self.contract_reload = true;
                }
            }
            SourceUpdate::Settings(_) => {
                if std::mem::take(&mut self.risk_reload) {
                    self.risk_loaded = false;
                }
                if std::mem::take(&mut self.contract_reload) {
                    self.contract_loaded = false;
                }
            }
            _ => {}
        }
    }

    /// Fills the forms from the stored settings once they are known (and after each save).
    pub fn load_forms(&mut self, snap: &UiSnapshot, window: &mut Window, cx: &mut Context<Shell>) {
        // Candidates follow the snapshot (replaced only when they changed) and the chosen exchange.
        self.c_symbol.sync(symbol_options::all_symbol_options(snap), window, cx);
        self.r_coins.sync(symbol_options::coin_options(snap), window, cx);
        if let Some(p) = &self.pending_manual {
            self.m_exchange = p.exchange;
        }
        if let Some(p) = &self.pending_cancel {
            self.x_exchange = p.exchange;
        }
        self.m_symbol.sync(symbol_options::symbol_options(snap, self.m_exchange), window, cx);
        self.x_symbol.sync(symbol_options::open_order_symbols(snap, self.x_exchange), window, cx);
        if let Some(p) = self.pending_manual.take() {
            self.m_symbol.set_value(&p.symbol, window, cx);
            set_val(&self.m_qty, p.quantity, window, cx);
            self.m_exchange = p.exchange;
            self.m_side = p.side;
            self.m_reduce = p.reduce_only;
            self.manual_confirm = None;
        }
        if let Some(p) = self.pending_cancel.take() {
            self.x_symbol.set_value(&p.symbol, window, cx);
            set_val(&self.x_id, p.order_id, window, cx);
            self.x_exchange = p.exchange;
        }
        if !self.settings_seen {
            return;
        }
        if !self.m_lev_loaded {
            // The manual order's leverage starts at the contract settings leverage (then user-editable).
            set_val(&self.m_lev, snap.settings.contract.leverage.normalize().to_string(), window, cx);
            self.m_lev_loaded = true;
        }
        if !self.contract_loaded {
            let f = ContractForm::from_template(&snap.settings.contract);
            set_val(&self.c_notional, f.notional, window, cx);
            set_val(&self.c_leverage, f.leverage, window, cx);
            set_val(&self.c_margin, f.margin, window, cx);
            self.contract_loaded = true;
        }
        if !self.risk_loaded {
            let form = RiskForm::from_settings(&snap.settings);
            for (f, i) in &self.r_fields {
                set_val(i, form.value(*f), window, cx);
            }
            self.r_over_on.clear();
            for ((ex, f), i) in &self.r_over {
                match form.override_value(*ex, *f) {
                    Some(v) => {
                        self.r_over_on.insert((*ex, *f));
                        set_val(i, v, window, cx);
                    }
                    None => set_val(i, risk_settings::field_text(&snap.settings.risk, *f), window, cx),
                }
            }
            self.r_allowed = form.allowed_exchanges.clone();
            self.r_coins.set_text(&form.allowed_coins, window, cx);
            self.risk_loaded = true;
        }
    }

    fn contract_form(&self, cx: &App) -> ContractForm {
        ContractForm { notional: val(&self.c_notional, cx), leverage: val(&self.c_leverage, cx), margin: val(&self.c_margin, cx), mode: self.c_mode }
    }

    fn risk_form(&self, base: &Settings, cx: &App) -> RiskForm {
        let mut form = RiskForm::from_settings(base);
        for (f, i) in &self.r_fields {
            form.set(*f, &val(i, cx));
        }
        for (ex, f) in OVERRIDE_FIELDS.iter().flat_map(|f| Exchange::ALL.map(|e| (e, *f))) {
            form.set_override(ex, f, false, base);
            if self.r_over_on.contains(&(ex, f)) {
                form.set_override(ex, f, true, base);
                if let Some(i) = self.r_over.get(&(ex, f)) {
                    form.set_override_value(ex, f, &val(i, cx));
                }
            }
        }
        form.allowed_exchanges = self.r_allowed.clone();
        form.allowed_coins = self.r_coins.text(cx);
        form
    }

    fn manual_form(&self, cx: &App) -> ManualForm {
        ManualForm { exchange: self.m_exchange, symbol: self.m_symbol.value(cx), side: self.m_side, quantity: val(&self.m_qty, cx), reduce_only: self.m_reduce, leverage: val(&self.m_lev, cx) }
    }

    fn cancel_form(&self, cx: &App) -> CancelForm {
        CancelForm { exchange: self.x_exchange, symbol: self.x_symbol.value(cx), order_id: val(&self.x_id, cx) }
    }
}

// ---- small widgets ----------------------------------------------------------------------------

fn btn(id: impl Into<ElementId>, label: impl Into<SharedString>, enabled: bool, on: ClickFn) -> Stateful<Div> {
    let d = div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme::BORDER))
        .bg(rgb(if enabled { theme::BG_CARD } else { theme::BG_SURFACE }))
        .child(small(label.into(), if enabled { theme::ACCENT } else { theme::TEXT_MUTED }));
    if enabled { d.cursor_pointer().on_click(on) } else { d }
}

fn field_row(label: impl Into<SharedString>, unit: &str, input: &In, error: Option<&String>) -> Div {
    field_row_badged(None, label, unit, input, error)
}

/// Box fill per direction; the arrow text always accompanies it so colour is never the only cue.
fn strict_badge(s: Strictness) -> Div {
    let fill = match s {
        Strictness::HigherStricter => theme::STRICT_HIGH,
        Strictness::LowerStricter => theme::STRICT_LOW,
        Strictness::Fact => theme::BG_SURFACE,
    };
    div().px_2().py_0p5().rounded_sm().border_1().border_color(rgb(theme::BORDER)).bg(rgb(fill)).flex_none().child(small(s.badge(), theme::TEXT_PRIMARY))
}

fn field_row_badged(strict: Option<Strictness>, label: impl Into<SharedString>, unit: &str, input: &In, error: Option<&String>) -> Div {
    let mut row = div().flex().gap_2().items_center();
    if let Some(s) = strict {
        row = row.child(div().w(rx(96.0)).child(strict_badge(s)));
    }
    row = row
        .child(small(label.into(), theme::TEXT_SECONDARY).w(rx(260.0)))
        .child(div().w(rx(160.0)).child(Input::new(input)))
        .child(small(unit.to_string(), theme::TEXT_MUTED));
    if let Some(e) = error {
        row = row.child(small(e.clone(), theme::NEGATIVE));
    }
    row
}

/// Body of the `?` dialog: formulas, qualify conditions, the merge rule, then every field.
fn risk_help_body() -> Div {
    let section = |t: &str| text(t.to_string(), theme::ACCENT);
    let mut d = div().flex().flex_col().gap_2().pr_2();
    d = d
        .child(section("Net Edge"))
        .child(small(risk_settings::NET_EDGE_FORMULA, theme::TEXT_PRIMARY))
        .child(small(risk_settings::REQUIRED_SPREAD_FORMULA, theme::TEXT_PRIMARY))
        .child(section("達標條件（全部成立）"));
    for c in risk_settings::QUALIFY_CONDITIONS {
        d = d.child(small(format!("・{c}"), theme::TEXT_SECONDARY));
    }
    d = d.child(section("每腿覆寫")).child(small(risk_settings::OVERRIDE_RULE, theme::TEXT_SECONDARY)).child(section("各欄位"));
    for f in GLOBAL_FIELDS {
        let h = f.help();
        let affects = h.affects.iter().map(|a| a.label()).collect::<Vec<_>>().join("、");
        let mut c = div().flex().flex_col().gap_1().p_2().rounded_sm().bg(rgb(theme::BG_SURFACE)).child(div().flex().gap_2().items_center().child(strict_badge(f.strictness())).child(small(f.key(), theme::TEXT_PRIMARY)));
        c = c.child(small(h.meaning, theme::TEXT_SECONDARY));
        if let Some(formula) = h.formula {
            c = c.child(small(format!("公式：{formula}"), theme::TEXT_MUTED));
        }
        c = c.child(small(format!("影響：{affects}"), theme::TEXT_MUTED));
        d = d.child(c);
    }
    d
}

/// Opens the help dialog (gpui-component `WindowExt::open_dialog`; the Root layer draws it).
fn open_risk_help(window: &mut Window, cx: &mut App) {
    use gpui_kit::component::WindowExt as _;
    window.open_dialog(cx, |dialog, window, _| {
        dialog
            .title("風控參數說明")
            .w(rx(640.0).to_pixels(window.rem_size()))
            .content(|content, _, _| content.child(div().id("risk-help-scroll").max_h(rx(520.0)).overflow_y_scroll().child(risk_help_body())))
    });
}

/// A symbol dropdown row; `unlisted` = names the selected values missing from the candidate list.
fn pick_row(label: impl Into<SharedString>, picker: impl IntoElement, loaded: bool, list_name: &str, unlisted: Option<String>) -> Div {
    let mut row = div()
        .flex()
        .gap_2()
        .items_center()
        .child(small(label.into(), theme::TEXT_SECONDARY).w(rx(260.0)))
        .child(div().w(rx(220.0)).child(picker));
    if !loaded {
        row = row.child(small("行情載入中（仍可直接輸入）", theme::TEXT_MUTED));
    }
    if let Some(u) = unlisted {
        row = row.child(small(format!("{u} 不在目前{list_name}清單中"), theme::WARNING));
    }
    row
}

fn warn_box(lines: Vec<String>) -> Div {
    let mut d = card();
    for l in lines {
        d = d.child(small(l, theme::WARNING));
    }
    d
}

fn mode_text(m: Option<ExecutionMode>) -> &'static str {
    match m {
        Some(m) => risk_settings::mode_label(m),
        None => "未知（引擎未啟動）",
    }
}

fn side_text(s: OrderSide) -> &'static str {
    match s {
        OrderSide::Buy => "BUY",
        OrderSide::Sell => "SELL",
    }
}

fn opt_money(d: Option<tong_funding_core::types::Decimal>) -> String {
    d.map_or_else(|| DASH.into(), |v| format::money(v, 2))
}

impl Shell {
    fn click(&self, cx: &mut Context<Self>, f: impl Fn(&mut Shell, &mut Context<Shell>) + 'static) -> ClickFn {
        Box::new(cx.listener(move |this: &mut Shell, _: &ClickEvent, _, cx| {
            f(this, cx);
            cx.notify();
        }))
    }

    /// The latest command replies (what the engine said, as is).
    pub(crate) fn replies_strip(&self) -> Div {
        let mut d = div().flex().flex_col().mb_2();
        for r in self.snap.replies.iter().rev().take(3) {
            let (t, c) = match &r.reply {
                CommandReply::Accepted => (format!("{} · 已受理（{} UTC）", r.label, format::utc_hms(r.at)), theme::POSITIVE),
                CommandReply::AlreadyPending => (format!("{} · 已有暫存配對", r.label), theme::WARNING),
                CommandReply::Rejected(why) => (format!("{} · 被拒絕：{why}", r.label), theme::NEGATIVE),
            };
            d = d.child(small(t, c));
        }
        d
    }

    // ---- scanner candidates -------------------------------------------------------------------

    pub(crate) fn candidate_cells(&self, rows: &[ScanRow]) -> Vec<CandidateCell> {
        rows.iter()
            .map(|r| CandidateCell {
                checked: self.trading.candidates.contains(&r.symbol),
                blocked: candidates::eligibility(r, &self.snap, self.now_ms).err().map(|b| b.text()),
            })
            .collect()
    }

    /// Clicks from the scanner table's "加入交易單" cells. Returns whether the list changed.
    pub(crate) fn drain_candidate_toggles(&mut self) -> bool {
        let clicked: Vec<String> = self.trading.toggles.borrow_mut().drain(..).collect();
        let mut changed = false;
        for sym in clicked {
            if let Some(row) = self.scanner.rows.iter().find(|r| r.symbol == sym) {
                changed |= self.trading.candidates.toggle(row, &self.snap, self.now_ms);
            }
        }
        changed
    }

    pub(crate) fn candidate_panel(&self, cx: &mut Context<Self>) -> Div {
        let views = candidates::views(&self.trading.candidates, &self.scanner, &self.snap, self.now_ms);
        let mut panel = card().child(text("Candidate List · 待加入交易單", theme::TEXT_PRIMARY)).child(small("勾選表格的「加入交易單」欄；只存在本次執行的記憶體中，重啟後清空。加入只建立 PREPARED 配對，不送出任何訂單。", theme::TEXT_MUTED));
        if views.is_empty() {
            return panel.child(small("尚未勾選任何候選", theme::TEXT_MUTED));
        }
        for v in &views {
            for ex in [v.long, v.short] {
                self.source.request_leverage_cap(ex, &v.symbol, v.notional);
            }
            let countdown = v.settlement_ms.map(|t| format!("{} UTC · 倒數 {}", format::utc_hms(t), format::hms(t - self.now_ms))).unwrap_or_else(|| DASH.into());
            let sym = v.symbol.clone();
            let mut line = div()
                .flex()
                .gap_3()
                .items_center()
                .child(small(v.symbol.clone(), theme::TEXT_PRIMARY).w(rx(110.0)))
                .child(small(format!("L {} / S {}", v.long.name(), v.short.name()), theme::TEXT_SECONDARY))
                .child(small(format!("Gross {}", v.gross_spread.map_or_else(|| DASH.into(), format::rate_pct)), theme::TEXT_SECONDARY))
                .child(small(format!("Net Edge {}", v.net_edge_pct.map_or_else(|| DASH.into(), |n| format::fixed(n, 4))), theme::TEXT_SECONDARY))
                .child(small(countdown, theme::TEXT_SECONDARY))
                .child(small(format!("每腿 {} USDT · {}× · Margin {}", format::money(v.notional, 2), v.leverage.normalize(), format::money(v.margin, 2)), theme::TEXT_SECONDARY));
            line = line.child(small(v.cap.text(), theme::TEXT_SECONDARY));
            line = match &v.valid {
                Ok(()) => line.child(small("可加入", theme::POSITIVE)),
                Err(why) => line.child(small(why.clone(), theme::WARNING)),
            };
            line = line.child(btn(SharedString::from(format!("cand-rm-{sym}")), "移除", true, self.click(cx, move |this, cx| {
                this.trading.candidates.remove(&sym);
                this.sync_table(cx);
            })));
            panel = panel.child(line);
        }
        let any_valid = views.iter().any(|v| v.valid.is_ok());
        panel.child(btn("cand-add", "加入並前往交易單 →", any_valid, self.click(cx, |this, cx| {
            let n = candidates::add_to_staged(&this.trading.candidates, &this.scanner, &this.snap, this.now_ms, this.sink.as_ref());
            if n > 0 {
                // Kept until the engine's snapshot shows the pairs (a refused one stays listed).
                let added: Vec<String> = candidates::views(&this.trading.candidates, &this.scanner, &this.snap, this.now_ms).into_iter().filter(|v| v.valid.is_ok()).map(|v| v.symbol).collect();
                for s in added {
                    this.trading.candidates.remove(&s);
                }
                this.sync_table(cx);
                this.go(Page::StagedOrders);
            }
        })))
    }

    // ---- staged orders --------------------------------------------------------------------------

    pub(crate) fn staged_orders_page(&mut self, cx: &mut Context<Self>) -> Div {
        let vm = staged_orders::build(&self.snap, &self.trading.staged_sel, self.now_ms);
        let trigger = vm.trigger_mode.map_or("未知".to_string(), |t| format!("{t:?}").to_uppercase());
        let header = div()
            .flex()
            .justify_between()
            .items_center()
            .child(title("交易單", "Staged Orders"))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .items_center()
                    .child(small(format!("模板：{} · 市價單 · 暫存 {} 筆 · 已選 {} 筆", vm.mode_label(), vm.staged_count, vm.selected_count), theme::TEXT_SECONDARY))
                    .child(small(format!("trigger_mode {trigger}"), theme::TEXT_SECONDARY))
                    .child(btn("trigger-toggle", "切換 AUTO / MANUAL", vm.trigger_mode.is_some(), self.click(cx, |this, _| {
                        let vm = staged_orders::build(&this.snap, &this.trading.staged_sel, this.now_ms);
                        staged_orders::toggle_trigger_mode(&vm, this.sink.as_ref());
                    }))),
            );
        let all = staged_orders::select_all(&vm);
        let controls = div()
            .flex()
            .gap_2()
            .items_center()
            .child(btn("sel-all", "全選", true, self.click(cx, move |this, _| this.trading.staged_sel = all.clone())))
            .child(btn("sel-none", "全不選", true, self.click(cx, |this, _| this.trading.staged_sel = staged_orders::select_none())))
            .child(small(format!("已選 {} 筆 · {} 腿 · 總 Notional {} · 總 Margin {}", vm.summary.pairs, vm.summary.legs, format::money(vm.summary.notional, 2), format::money(vm.summary.margin, 2)), theme::TEXT_PRIMARY));
        let mut margins = div().flex().gap_4();
        for (ex, t) in &vm.margins {
            margins = margins.child(small(format!("{} 可用保證金：{t}", ex.name()), theme::TEXT_SECONDARY));
        }
        let enabled = vm.disabled.is_empty();
        let mut submit = div().flex().gap_3().items_center().child(btn("submit-selected", "一鍵送出已選取", enabled, self.click(cx, |this, _| {
            let vm = staged_orders::build(&this.snap, &this.trading.staged_sel, this.now_ms);
            this.trading.staged_confirm = staged_orders::open_confirm(&vm);
        })));
        for r in &vm.disabled {
            submit = submit.child(small(r.text(), theme::WARNING));
        }
        let mut page = div().flex().flex_col().gap_3().child(header).child(controls).child(margins).child(submit);
        if let Some(n) = &self.trading.staged_note {
            page = page.child(small(n.clone(), theme::WARNING));
        }
        if let Some(p) = self.trading.staged_confirm.clone() {
            let mut c = card().border_color(rgb(theme::WARNING)).child(text(format!("確認送出 {} 筆配對（{} 腿）", p.pairs.len(), p.legs.len()), theme::TEXT_PRIMARY)).child(small(p.env_text.clone(), theme::WARNING));
            for l in &p.legs {
                c = c.child(small(
                    format!("{} · {} · {} · {} · Notional {} · {}× · Margin {}", l.exchange.name(), l.symbol, side_text(l.side), l.qty_text, format::money(l.notional, 2), l.leverage.normalize(), format::money(l.margin, 2)),
                    theme::TEXT_SECONDARY,
                ));
            }
            c = c.child(
                div()
                    .flex()
                    .gap_2()
                    .child(btn("confirm-yes", "確認送出", true, self.click(cx, |this, _| {
                        if let Some(p) = this.trading.staged_confirm.take() {
                            let out = staged_orders::confirm(&p, &this.snap, this.sink.as_ref());
                            this.trading.staged_note = (!out.excluded.is_empty()).then(|| format!("已排除（已不是 PREPARED）：{}", out.excluded.join("、")));
                        }
                    })))
                    .child(btn("confirm-no", "取消", true, self.click(cx, |this, _| this.trading.staged_confirm = None))),
            );
            page = page.child(c);
        }
        let mut table = card().child(small("勾選 · 標的 · Gross · Net Edge % · LONG · SHORT · 每腿 Notional · Margin · 槓桿 · 進場倒數", theme::TEXT_MUTED));
        if vm.rows.is_empty() {
            table = table.child(small("沒有暫存配對（於掃幣頁加入）", theme::TEXT_MUTED));
        }
        for r in &vm.rows {
            if let Some(n) = r.notional {
                for ex in [r.long, r.short] {
                    self.source.request_leverage_cap(ex, &r.symbol, n);
                }
            }
            let uuid = r.uuid.clone();
            let mark = match (&r.selectable, r.selected) {
                (Err(_), _) => "☐",
                (Ok(()), true) => "☑",
                (Ok(()), false) => "☐",
            };
            let check = btn(SharedString::from(format!("sel-{uuid}")), mark, r.selectable.is_ok(), self.click(cx, {
                let uuid = uuid.clone();
                move |this, _| {
                    if !this.trading.staged_sel.remove(&uuid) {
                        this.trading.staged_sel.insert(uuid.clone());
                    }
                }
            }));
            let mut line = div()
                .flex()
                .gap_3()
                .items_center()
                .child(check)
                .child(small(r.symbol.clone(), theme::TEXT_PRIMARY).w(rx(100.0)))
                .child(small(r.gross_spread.map_or_else(|| DASH.into(), format::rate_pct), theme::TEXT_SECONDARY))
                .child(small(r.net_edge_pct.map_or_else(|| DASH.into(), |n| format::fixed(n, 4)), theme::TEXT_SECONDARY))
                .child(small(format!("L {} {}", r.long.name(), r.long_qty.text()), theme::TEXT_SECONDARY))
                .child(small(format!("S {} {}", r.short.name(), r.short_qty.text()), theme::TEXT_SECONDARY))
                .child(small(opt_money(r.notional), theme::TEXT_SECONDARY))
                .child(small(opt_money(r.margin), theme::TEXT_SECONDARY))
                .child(small(r.leverage.map_or_else(|| DASH.into(), |l| format!("{}×", l.normalize())), theme::TEXT_SECONDARY))
                .child(small(r.entry_in_ms.map_or_else(|| DASH.into(), format::hms), theme::TEXT_SECONDARY));
            if let Some(cap) = &r.cap {
                line = line.child(small(cap.text(), theme::TEXT_SECONDARY));
            }
            if let Err(why) = &r.selectable {
                line = line.child(small(why.clone(), theme::WARNING));
            }
            line = line.child(btn(SharedString::from(format!("rm-{uuid}")), "移除", true, self.click(cx, move |this, _| {
                this.trading.staged_sel.remove(&uuid);
                this.sink.send("移除暫存配對".into(), Command::CancelPrepared { pair: uuid.clone(), reason: "使用者於交易單移除".into() });
            })));
            // trade-cost-estimate: the pair's estimated cost (or why there is none), under its row.
            let estimate_ok = matches!(r.cost, CostView::Estimate(_));
            let mut cost = div().flex().flex_col().pl(rx(28.0));
            for (i, t) in r.cost.lines().into_iter().enumerate() {
                let color = if estimate_ok && i < 4 { theme::TEXT_MUTED } else { theme::WARNING };
                cost = cost.child(small(t, color));
            }
            table = table.child(div().flex().flex_col().gap_1().child(line).child(cost));
        }
        page = page.child(table);

        if !vm.running.is_empty() {
            let mut run = card().child(text("進行中（RECONCILED）", theme::TEXT_PRIMARY));
            for r in &vm.running {
                let row = r.clone();
                let mut line = div().flex().gap_3().items_center().child(small(r.symbol.clone(), theme::TEXT_PRIMARY)).child(small(format!("平倉倒數 {}", format::hms(r.exit_in_ms)), theme::TEXT_SECONDARY));
                line = if r.close_now {
                    line.child(btn(SharedString::from(format!("close-now-{}", r.uuid)), "立即平倉", true, self.click(cx, move |this, _| this.trading.close_now_confirm = Some(row.clone()))))
                } else {
                    line.child(small(r.action_text(), theme::TEXT_MUTED))
                };
                run = run.child(line);
            }
            if let Some(r) = self.trading.close_now_confirm.clone() {
                run = run.child(small(format!("確認立即平倉 {}？兩腿以 reduce-only 平倉，數量由 engine 讀取實際持倉。", r.symbol), theme::WARNING)).child(
                    div()
                        .flex()
                        .gap_2()
                        .child(btn("close-now-yes", "確認平倉", true, self.click(cx, |this, _| {
                            if let Some(r) = this.trading.close_now_confirm.take() {
                                staged_orders::send_close_now(&r, this.sink.as_ref());
                            }
                        })))
                        .child(btn("close-now-no", "取消", true, self.click(cx, |this, _| this.trading.close_now_confirm = None))),
                );
            }
            page = page.child(run);
        }

        if !vm.manual.is_empty() {
            let mut m = card().border_color(rgb(theme::NEGATIVE)).child(text("需人工處理（系統不會自動補救）", theme::NEGATIVE));
            for h in &vm.manual {
                let mut legs = div().flex().gap_3();
                for (ex, q) in &h.legs {
                    legs = legs.child(small(
                        match q {
                            Ok(q) => format!("{} 持倉 {}", ex.name(), q.normalize()),
                            Err(e) => e.clone(),
                        },
                        theme::TEXT_SECONDARY,
                    ));
                }
                let hc = h.clone();
                let hc2 = h.clone();
                let mut actions = div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(btn(SharedString::from(format!("mclose-{}", h.uuid)), h.actions()[0], h.close.is_ok(), self.click(cx, move |this, _| this.trading.close_confirm = staged_orders::request_close(&hc))))
                    .child(btn(SharedString::from(format!("mconfirm-{}", h.uuid)), h.actions()[1], h.confirm_closed.is_ok(), self.click(cx, move |this, _| {
                        staged_orders::send_confirm_closed(&hc2, this.sink.as_ref());
                    })));
                if let Err(why) = &h.confirm_closed {
                    actions = actions.child(small(why.clone(), theme::WARNING));
                }
                m = m.child(small(format!("{} · {} · {}", h.symbol, h.state, if h.simulated { "SIMULATION" } else { "EXCHANGE_DEMO" }), theme::TEXT_PRIMARY)).child(legs).child(actions);
            }
            if let Some(c) = self.trading.close_confirm.clone() {
                let mut panel = card().child(small(format!("確認人工要求平倉 {}（reduce-only，數量取自最新持倉）", c.symbol), theme::WARNING));
                for l in &c.legs {
                    panel = panel.child(small(format!("{} · {} · {}", l.exchange.name(), l.symbol, l.quantity.normalize()), theme::TEXT_SECONDARY));
                }
                panel = panel.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(btn("mclose-yes", "確認平倉", true, self.click(cx, |this, _| {
                            if let Some(c) = this.trading.close_confirm.take() {
                                staged_orders::send_close(&c, this.sink.as_ref());
                            }
                        })))
                        .child(btn("mclose-no", "取消", true, self.click(cx, |this, _| this.trading.close_confirm = None))),
                );
                m = m.child(panel);
            }
            page = page.child(m);
        }

        let mut hist = card().child(text("上次執行結果（歷史紀錄，非目前選取項目）", theme::TEXT_PRIMARY));
        if vm.last_results.is_empty() {
            hist = hist.child(small("尚無執行紀錄", theme::TEXT_MUTED));
        }
        for a in &vm.last_results {
            hist = hist.child(small(format!("{} UTC · {}", format::utc_hms(a.at), a.headline), if a.needs_manual { theme::NEGATIVE } else { theme::TEXT_PRIMARY }));
            for l in &a.legs {
                hist = hist.child(small(format!("  {} · {} · {} · {} · {}", l.leg, l.exchange, side_text(l.side), l.status.text(), l.order_id_text), theme::TEXT_SECONDARY));
            }
        }
        page.child(hist)
    }

    // ---- contract settings ----------------------------------------------------------------------

    pub(crate) fn contract_settings_page(&mut self, cx: &mut Context<Self>) -> Div {
        let mode = self.snap.engine.as_ref().map(|e| e.execution_mode);
        let form = self.trading.contract_form(cx);
        let vm = contract_settings::evaluate(&form, &self.snap.settings, mode);
        let symbol = self.trading.c_symbol.value(cx).trim().to_ascii_uppercase();
        if symbol != self.trading.c_requested {
            for ex in Exchange::ALL {
                self.source.request_rules(ex, &symbol);
            }
            self.trading.c_requested = symbol.clone();
        }
        let mode_btn = |this: &Shell, cx: &mut Context<Shell>, m: CalcMode, label: &'static str, id: &'static str| {
            let on = this.trading.c_mode == m;
            btn(id, format!("{} {label}", if on { "●" } else { "○" }), true, this.click(cx, move |this, _| this.trading.c_mode = m))
        };
        let mut form_card = card()
            .child(small("預設下單模板：Target Notional 為「每一腿」的名目本金（一對 LONG + SHORT 為兩倍）", theme::TEXT_SECONDARY))
            .child(div().flex().gap_2().child(mode_btn(self, cx, CalcMode::LeverageToMargin, "用槓桿反推保證金", "c-mode-l")).child(mode_btn(self, cx, CalcMode::MarginToLeverage, "用保證金反推槓桿", "c-mode-m")))
            .child(field_row("Target Notional（每腿）", "USDT", &self.trading.c_notional, None));
        form_card = match self.trading.c_mode {
            CalcMode::LeverageToMargin => form_card.child(field_row("Leverage", "×", &self.trading.c_leverage, None)).child(small(format!("Margin / leg：{}", opt_money(vm.margin)), theme::TEXT_PRIMARY)),
            CalcMode::MarginToLeverage => form_card.child(field_row("Margin（每腿）", "USDT", &self.trading.c_margin, None)).child(small(format!("Leverage：{}", vm.leverage.map_or_else(|| DASH.into(), |l| format!("{}×", l.normalize()))), theme::TEXT_PRIMARY)),
        };
        if let Some(f) = &vm.formula {
            form_card = form_card.child(small(f.clone(), theme::TEXT_SECONDARY));
        }
        form_card = form_card.child(small(format!("一對 LONG + SHORT：總名目本金 {} USDT · 合計保證金 {} USDT", opt_money(vm.pair_notional), opt_money(vm.pair_margin)), theme::TEXT_PRIMARY));
        for e in &vm.errors {
            form_card = form_card.child(small(e.clone(), theme::NEGATIVE));
        }
        if let Some(w) = &vm.leverage_warning {
            form_card = form_card.child(small(w.clone(), theme::WARNING));
        }
        if let Some(e) = &self.snap.settings.contract_error {
            form_card = form_card.child(small(format!("已儲存的模板無效：{e}"), theme::NEGATIVE));
        }
        form_card = form_card.child(btn("c-save", "儲存模板", vm.can_save, self.click(cx, |this, cx| {
            let mode = this.snap.engine.as_ref().map(|e| e.execution_mode);
            let vm = contract_settings::evaluate(&this.trading.contract_form(cx), &this.snap.settings, mode);
            contract_settings::save(&vm, this.sink.as_ref());
        })));
        let boundary = card()
            .child(text(format!("計算與執行邊界 · execution_mode {}", vm.mode_label), theme::TEXT_PRIMARY))
            .child(small(format!("每腿：Notional {} · Initial Margin {}", opt_money(vm.notional), opt_money(vm.margin)), theme::TEXT_SECONDARY))
            .child(small(format!("雙腿合計：Notional {} · Initial Margin {}", opt_money(vm.pair_notional), opt_money(vm.pair_margin)), theme::TEXT_SECONDARY))
            .child(small("預期數量：LONG / SHORT 兩腿同一幣數，名目本金為上限（不跨所加總）", theme::TEXT_SECONDARY))
            .child(small(contract_settings::QUOTE_NOTE, theme::TEXT_MUTED));
        let mut quote = card().child(pick_row("試算標的", self.trading.c_symbol.element("BTCUSDT"), self.trading.c_symbol.loaded(), "行情", self.trading.c_symbol.is_unlisted(cx).then(|| symbol.clone())));
        match vm.notional {
            Some(n) => {
                for q in contract_settings::quote(&symbol, n, &self.snap, self.now_ms) {
                    let price = q.price.map_or_else(|| DASH.into(), |p| p.normalize().to_string());
                    let age = q.observed_at.map_or_else(|| DASH.into(), |t| format!("{} 秒前", format::secs(self.now_ms - t)));
                    quote = quote.child(small(format!("{} · 現價 {price} · {age} · 預期數量 {}", q.exchange.name(), q.cell.text()), theme::TEXT_SECONDARY));
                }
                // Pairs trade one shared quantity (matched-leg-quantity); the rows above are single-leg references.
                quote = quote.child(small(contract_settings::pair_quote(&symbol, n, &self.snap, self.now_ms), theme::TEXT_PRIMARY));
            }
            None => quote = quote.child(small("Target Notional 無效，無法試算", theme::TEXT_MUTED)),
        }
        div().flex().flex_col().gap_3().child(title("合約設定", "Contract Settings")).child(form_card).child(boundary).child(quote)
    }

    // ---- risk settings --------------------------------------------------------------------------

    pub(crate) fn risk_settings_page(&mut self, cx: &mut Context<Self>) -> Div {
        let base = self.snap.settings.clone();
        let form = self.trading.risk_form(&base, cx);
        let vm = risk_settings::evaluate(&form, &base);
        let help_btn = div()
            .id("risk-help")
            .test_support()
            .px_2()
            .rounded_sm()
            .border_1()
            .border_color(rgb(theme::BORDER))
            .bg(rgb(theme::BG_CARD))
            .cursor_pointer()
            .child(text("?", theme::ACCENT))
            .on_click(|_, window, cx| open_risk_help(window, cx));
        let mut page = div().flex().flex_col().gap_3().child(div().flex().items_center().gap_3().child(title("風控設定", "Risk Management")).child(help_btn));
        if let Some(t) = &vm.incomplete_text {
            page = page.child(warn_box(vec![t.clone(), "設定不完整時交易單與 EXCHANGE_DEMO 會被禁用".into()]));
        }
        if let Some(e) = &base.error {
            page = page.child(warn_box(vec![format!("已儲存的風控設定讀取失敗：{e}")]));
        }
        let mut global = card().child(text("Global Limits · Layer 1 · Net Edge", theme::TEXT_PRIMARY));
        for f in GLOBAL_FIELDS {
            let label = match f {
                Field::TakerFee(e) => format!("{} Taker 費率 · 必填（請依帳戶手續費等級實填）", e.name()),
                Field::NetEdgeThresholdPct | Field::EstSlippagePct => format!("{} · 必填", f.label()),
                _ => f.label().to_string(),
            };
            let key = f.key();
            if let Some(i) = self.trading.r_fields.get(&f) {
                global = global.child(field_row_badged(Some(f.strictness()), format!("{label}（{key}）"), f.unit(), i, vm.field_errors.get(&key)));
            }
            if f == Field::StaleDataThresholdMs {
                global = global.child(small(risk_settings::STALE_NOTE, theme::TEXT_MUTED));
            }
            if f == Field::MaxConcurrentPairs {
                let open = self.snap.pairs.iter().filter(|p| p.state.is_ok()).count();
                global = global.child(small(format!("目前 {open} / {} 組", base.risk.max_concurrent_pairs), theme::TEXT_MUTED));
            }
        }
        let mut allowed = div().flex().gap_2().items_center().child(small("allowed_exchanges", theme::TEXT_SECONDARY).w(rx(260.0)));
        for ex in Exchange::ALL {
            let on = self.trading.r_allowed.contains(&ex);
            allowed = allowed.child(btn(SharedString::from(format!("allow-{}", ex.name())), format!("{} {}", if on { "☑" } else { "☐" }, ex.name()), true, self.click(cx, move |this, _| {
                if !this.trading.r_allowed.remove(&ex) {
                    this.trading.r_allowed.insert(ex);
                }
            })));
        }
        global = global.child(allowed).child(pick_row("allowed_coins（可多選，空白 = 不限制）", self.trading.r_coins.element("不限制"), self.trading.r_coins.loaded(), "行情", Some(self.trading.r_coins.unlisted(cx).join(", ")).filter(|u| !u.is_empty()))).child(small(vm.formula.clone(), theme::TEXT_SECONDARY));
        for (k, e) in &vm.field_errors {
            if !GLOBAL_FIELDS.iter().any(|f| &f.key() == k) {
                global = global.child(small(e.clone(), theme::NEGATIVE));
            }
        }
        global = global.child(btn("risk-save", "儲存風控設定", vm.can_save, self.click(cx, |this, cx| {
            let base = this.snap.settings.clone();
            let vm = risk_settings::evaluate(&this.trading.risk_form(&base, cx), &base);
            risk_settings::save(&vm, this.sink.as_ref());
        })));
        page = page.child(global);

        // Execution mode (SIMULATION / EXCHANGE_DEMO only).
        let current = self.snap.engine.as_ref().map(|e| e.execution_mode);
        let demo_ok = risk_settings::demo_option(&base, self.snap.demo_keys.as_ref());
        let mut mode = card().child(text(format!("執行模式 · 目前 {}", mode_text(current)), theme::TEXT_PRIMARY));
        for m in MODE_OPTIONS {
            let on = current == Some(m);
            let enabled = current.is_some() && (m == ExecutionMode::Simulation || demo_ok.is_ok());
            mode = mode.child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(btn(SharedString::from(format!("mode-{}", risk_settings::mode_label(m))), format!("{} {}", if on { "●" } else { "○" }, risk_settings::mode_label(m)), enabled, self.click(cx, move |this, _| {
                        let Some(cur) = this.snap.engine.as_ref().map(|e| e.execution_mode) else { return };
                        match risk_settings::request_mode(cur, m, &this.snap.settings, this.snap.demo_keys.as_ref(), this.sink.as_ref()) {
                            Ok(risk_settings::ModeStep::Confirm(t)) => this.trading.mode_confirm = Some(t),
                            Ok(risk_settings::ModeStep::Sent | risk_settings::ModeStep::Unchanged) => this.trading.mode_error = None,
                            Err(why) => this.trading.mode_error = Some(why),
                        }
                    })))
                    .child(small(risk_settings::mode_description(m), theme::TEXT_MUTED)),
            );
        }
        if let Err(why) = &demo_ok {
            mode = mode.child(small(format!("EXCHANGE_DEMO 不可選：{why}"), theme::WARNING));
        }
        if let Some(e) = &self.trading.mode_error {
            mode = mode.child(small(e.clone(), theme::NEGATIVE));
        }
        if let Some(t) = self.trading.mode_confirm {
            mode = mode.child(small(format!("確認切換到 {}？將對 demo / testnet 帳戶真實下單（不涉及真錢）。既有倉位與 trigger_mode 不變。", risk_settings::mode_label(t)), theme::WARNING)).child(
                div()
                    .flex()
                    .gap_2()
                    .child(btn("mode-yes", "確認切換", true, self.click(cx, |this, _| {
                        if let Some(t) = this.trading.mode_confirm.take() {
                            risk_settings::send_mode(t, this.sink.as_ref());
                        }
                    })))
                    .child(btn("mode-no", "取消", true, self.click(cx, |this, _| this.trading.mode_confirm = None))),
            );
        }
        page = page.child(mode);

        // Per-exchange overrides (exactly the nine fields) and the effective preview.
        let mut ov = div().flex().flex_wrap().gap_3();
        for ex in Exchange::ALL {
            let mut c = card().w(rx(580.0)).child(text(format!("{} 覆寫", ex.name()), theme::TEXT_PRIMARY));
            for f in OVERRIDE_FIELDS {
                let on = self.trading.r_over_on.contains(&(ex, f));
                let toggle = btn(SharedString::from(format!("ov-{}-{}", ex.name(), f.key())), if on { "☑ 獨立設定" } else { "☐ 獨立設定" }, true, self.click(cx, move |this, _| {
                    if !this.trading.r_over_on.remove(&(ex, f)) {
                        this.trading.r_over_on.insert((ex, f));
                    }
                }));
                let mut row = div().flex().gap_2().items_center().child(div().w(rx(96.0)).child(strict_badge(f.strictness()))).child(small(f.key(), theme::TEXT_SECONDARY).w(rx(170.0))).child(toggle);
                row = if on {
                    match self.trading.r_over.get(&(ex, f)) {
                        Some(i) => row.child(div().w(rx(110.0)).child(Input::new(i))),
                        None => row,
                    }
                } else {
                    row.child(small(format!("繼承 {}", form.value(f)), theme::TEXT_MUTED))
                };
                c = c.child(row);
            }
            ov = ov.child(c);
        }
        let mut preview = card().child(text("配對生效值預覽（與送單前檢查同一個 effective_for_pair）", theme::TEXT_PRIMARY));
        for p in &vm.preview {
            let e = &p.effective;
            preview = preview.child(small(
                format!(
                    "{}×{} · max_leverage {} · drift {} · stale {}ms · timeout {}s · imbalance {} · min vol {} · threshold {} · slippage {} · safety {}",
                    p.long.name(),
                    p.short.name(),
                    e.max_leverage.normalize(),
                    e.max_price_drift_pct.normalize(),
                    e.stale_data_threshold_ms,
                    e.order_timeout_seconds,
                    e.max_leg_imbalance_pct.normalize(),
                    e.min_24h_volume_usdt.normalize(),
                    e.net_edge_threshold_pct.map_or("未設定".into(), |v| v.normalize().to_string()),
                    e.est_slippage_pct.map_or("未設定".into(), |v| v.normalize().to_string()),
                    e.safety_margin_pct.normalize()
                ),
                theme::TEXT_SECONDARY,
            ));
        }
        page.child(ov).child(preview)
    }

    // ---- manual order ---------------------------------------------------------------------------

    /// "目前持倉": one row per non-zero position with a button that only fills the form.
    fn position_picker(&self, cx: &mut Context<Self>) -> Div {
        let mut d = div().flex().flex_col().gap_1().child(small("目前持倉（點「帶入平倉」只會填入表單，仍須確認才送出）", theme::TEXT_SECONDARY));
        for list in manual_order::open_positions(&self.snap) {
            let name = list.exchange.name();
            let age = list.fetched_at.map(|t| format!(" · 更新於 {} 秒前", ((self.snap.engine.as_ref().map_or(t, |e| e.now_ms) - t) / 1000).max(0))).unwrap_or_default();
            match list.state {
                PickState::NotQueried => d = d.child(small(format!("{name} 尚未查詢持倉"), theme::WARNING)),
                PickState::Failed(e) => d = d.child(small(format!("{name} 持倉讀取失敗：{e}"), theme::NEGATIVE)),
                PickState::Rows { rows, incomplete } => {
                    if incomplete {
                        d = d.child(small(format!("{name} 清單可能不完整"), theme::WARNING));
                    }
                    if rows.is_empty() {
                        d = d.child(small(format!("{name} 目前沒有持倉{age}"), theme::TEXT_SECONDARY));
                    }
                    for r in rows {
                        let pos = AccountPosition { exchange: r.exchange, symbol: r.symbol.clone(), quantity: r.quantity };
                        let label = format!("{name} · {} · {} {}{age}", r.symbol, if r.is_long { "多" } else { "空" }, r.quantity.abs().normalize());
                        d = d.child(div().flex().gap_2().items_center().child(small(label, theme::TEXT_PRIMARY)).child(btn(
                            SharedString::from(format!("m-pick-{name}-{}", r.symbol)),
                            "帶入平倉",
                            true,
                            self.click(cx, move |this, _| this.trading.pending_manual = Some(manual_order::close_prefill(&pos))),
                        )));
                    }
                }
            }
        }
        d
    }

    /// "目前掛單": click fills the cancel form; orders without an id are shown but not pickable.
    fn order_picker(&self, cx: &mut Context<Self>) -> Div {
        let mut d = div().flex().flex_col().gap_1().child(small("目前掛單（點選只會填入表單）", theme::TEXT_SECONDARY));
        for list in manual_order::open_orders(&self.snap) {
            let name = list.exchange.name();
            match list.state {
                PickState::NotQueried => d = d.child(small(format!("{name} 尚未查詢掛單"), theme::WARNING)),
                PickState::Failed(e) => d = d.child(small(format!("{name} 掛單讀取失敗：{e}"), theme::NEGATIVE)),
                PickState::Rows { rows, incomplete } => {
                    if incomplete {
                        d = d.child(small(format!("{name} 清單可能不完整"), theme::WARNING));
                    }
                    if rows.is_empty() {
                        d = d.child(small(format!("{name} 目前沒有掛單"), theme::TEXT_SECONDARY));
                    }
                    for (i, r) in rows.into_iter().enumerate() {
                        let label = format!("{name} · {} · 剩餘 {} · {}", r.symbol, r.remaining.normalize(), r.order_id.as_deref().unwrap_or("無 Order ID，無法由此撤單"));
                        let row = div().flex().gap_2().items_center().child(small(label, if r.order_id.is_some() { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY }));
                        d = d.child(match manual_order::cancel_prefill(&r) {
                            Some(p) => row.child(btn(SharedString::from(format!("x-pick-{name}-{i}")), "帶入撤單", true, self.click(cx, move |this, _| this.trading.pending_cancel = Some(p.clone())))),
                            None => row,
                        });
                    }
                }
            }
        }
        d
    }

    pub(crate) fn manual_order_page(&mut self, cx: &mut Context<Self>) -> Div {
        let form = self.trading.manual_form(cx);
        let cancel_form = self.trading.cancel_form(cx);
        let sym = form.symbol.trim().to_ascii_uppercase();
        if sym != self.trading.m_requested {
            for ex in [Exchange::Binance, Exchange::Bybit] {
                self.source.request_rules(ex, &sym);
            }
            self.trading.m_requested = sym;
        }
        let vm = manual_order::build(&form, &cancel_form, &self.snap);
        if !form.reduce_only
            && let Some(n) = vm.est_notional
        {
            self.source.request_leverage_cap(form.exchange, &form.symbol.trim().to_ascii_uppercase(), manual_order::cap_request_notional(n));
        }
        let warning = warn_box(vec![
            format!("⚠ {DEBUG_WARNING}：單腿下單會造成未避險曝險；標準流程為「掃幣 → 交易單」。"),
            vm.env_text.clone(),
        ]);
        let mut panels = div().flex().gap_2().items_center().child(small("交易所", theme::TEXT_SECONDARY));
        for (ex, state) in &vm.panels {
            let ex = *ex;
            let on = self.trading.m_exchange == ex;
            panels = panels.child(btn(SharedString::from(format!("m-ex-{}", ex.name())), format!("{} {}", if on { "●" } else { "○" }, ex.name()), state.is_ok(), self.click(cx, move |this, _| {
                this.trading.m_exchange = ex;
                this.trading.x_exchange = ex;
            })));
            if let Err(why) = state {
                panels = panels.child(small(format!("{}：{why}", ex.name()), theme::WARNING));
            }
        }
        let side = |this: &Shell, cx: &mut Context<Shell>, s: OrderSide| {
            let on = this.trading.m_side == s;
            btn(SharedString::from(format!("m-side-{}", side_text(s))), format!("{} {}", if on { "●" } else { "○" }, side_text(s)), true, this.click(cx, move |this, _| this.trading.m_side = s))
        };
        let reduce = btn("m-reduce", if self.trading.m_reduce { "☑ reduce_only" } else { "☐ reduce_only" }, true, self.click(cx, |this, _| this.trading.m_reduce = !this.trading.m_reduce));
        let mut order = card()
            .child(text("單腿下單 · 市價單", theme::TEXT_PRIMARY))
            .child(self.position_picker(cx))
            .child(panels)
            .child(pick_row("Symbol", self.trading.m_symbol.element("BTCUSDT"), self.trading.m_symbol.loaded(), "行情", self.trading.m_symbol.is_unlisted(cx).then(|| self.trading.m_symbol.value(cx))))
            .child(div().flex().gap_2().child(side(self, cx, OrderSide::Buy)).child(side(self, cx, OrderSide::Sell)).child(reduce))
            .child(field_row("Quantity", "", &self.trading.m_qty, None))
            .child(field_row("Leverage", "×（開倉單必填；reduce_only 不使用）", &self.trading.m_lev, None))
            .child(small(
                vm.cap_text.clone().unwrap_or_else(|| if form.reduce_only { "reduce_only：不設定槓桿".to_string() } else { "槓桿上限：—".to_string() }),
                theme::TEXT_SECONDARY,
            ))
            .child(small(
                match &vm.rounded {
                    Ok(q) => format!("取整後數量 {} · 估計 Notional {}", q.normalize(), opt_money(vm.est_notional)),
                    Err(e) => e.clone(),
                },
                if vm.rounded.is_ok() { theme::TEXT_PRIMARY } else { theme::WARNING },
            ));
        let mut submit = div().flex().gap_2().items_center().child(btn("m-submit", "Submit Order", vm.submit_disabled.is_empty(), self.click(cx, |this, cx| {
            let form = this.trading.manual_form(cx);
            let vm = manual_order::build(&form, &this.trading.cancel_form(cx), &this.snap);
            this.trading.manual_confirm = manual_order::open_confirm(&vm, &form);
        })));
        for why in &vm.submit_disabled {
            submit = submit.child(small(why.clone(), theme::WARNING));
        }
        order = order.child(submit);
        if let Some(c) = self.trading.manual_confirm.clone() {
            order = order.child(
                card()
                    .border_color(rgb(theme::WARNING))
                    .child(small(format!("確認：{} · {} · {} · {} · 估計 Notional {}{}", c.exchange.name(), c.symbol, side_text(c.side), c.qty_text, opt_money(c.est_notional), if c.reduce_only { " · reduce_only" } else { "" }), theme::TEXT_PRIMARY))
                    .child(small(
                        match (c.reduce_only, c.leverage) {
                            (false, Some(l)) => format!("槓桿 {}×（送單前先設定到交易所）· {}", l.normalize(), c.cap_text.clone().unwrap_or_default()),
                            _ => "不設定槓桿".to_string(),
                        },
                        theme::TEXT_PRIMARY,
                    ))
                    .child(small(c.env_text.clone(), theme::WARNING))
                    .child(small(c.warning, theme::WARNING))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(btn("m-yes", "確認送出", true, self.click(cx, |this, _| {
                                if let Some(c) = this.trading.manual_confirm.take() {
                                    manual_order::confirm(&c, this.sink.as_ref());
                                }
                            })))
                            .child(btn("m-no", "取消", true, self.click(cx, |this, _| this.trading.manual_confirm = None))),
                    ),
            );
        }
        let mut cancel = card()
            .child(text(format!("撤單 · {}", self.trading.x_exchange.name()), theme::TEXT_PRIMARY))
            .child(self.order_picker(cx))
            .child(pick_row("Symbol", self.trading.x_symbol.element("BTCUSDT"), true, "掛單", self.trading.x_symbol.is_unlisted(cx).then(|| self.trading.x_symbol.value(cx))))
            .child(field_row("Order ID（client_order_id）", "", &self.trading.x_id, None));
        let mut crow = div().flex().gap_2().items_center().child(btn("x-cancel", "Cancel", vm.cancel_disabled.is_empty(), self.click(cx, |this, cx| {
            let form = this.trading.cancel_form(cx);
            let vm = manual_order::build(&this.trading.manual_form(cx), &form, &this.snap);
            manual_order::cancel(&vm, &form, this.sink.as_ref());
        })));
        for why in &vm.cancel_disabled {
            crow = crow.child(small(why.clone(), theme::WARNING));
        }
        cancel = cancel.child(crow);
        let mut results = card().child(text("結果（如實顯示交易所 / 模擬器的回應）", theme::TEXT_PRIMARY));
        for r in &vm.results {
            results = results.child(small(format!("{} UTC · {}", format::utc_hms(r.at), r.text), if r.ok { theme::POSITIVE } else { theme::NEGATIVE }));
        }
        div().flex().flex_col().gap_3().child(title("手動下單", "Debug Tool")).child(warning).child(order).child(cancel).child(results)
    }
}
