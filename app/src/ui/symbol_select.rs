//! searchable-symbol-inputs: searchable dropdowns for symbols (single) and coins (multiple).
//!
//! Spike result (task 2.1): `SearchableVec` filters by `matches` only and cannot insert a row, so
//! a custom delegate rebuilds its rows in `perform_search` from the pure
//! [`symbol_options::options_with_query`] - the typed text becomes a first row "使用 NEWUSDT", so a
//! value that is not in the market list can still be selected. The value is read by polling
//! (`value` / `values`), like the plain inputs these replace.

use gpui_kit::component::combobox::{Combobox, ComboboxState};
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::IndexPath;
use gpui_kit::component::searchable_list::{SearchableListDelegate, SearchableListItem};
use gpui_kit::*;

use super::shell::Shell;
use super::symbol_options::{self, SymbolOption};

/// A dropdown row; `title` (also what the trigger shows) is the bare value.
#[derive(Clone)]
pub struct SymItem(SymbolOption);

impl SearchableListItem for SymItem {
    type Value = String;

    fn title(&self) -> SharedString {
        self.0.value.clone().into()
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let text = if self.0.free { format!("使用 {}", self.0.value) } else { self.0.value.clone() };
        SharedString::from(text)
    }

    fn value(&self) -> &String {
        &self.0.value
    }
}

/// Rows = the candidate list (plus the currently selected values that are not in it) filtered by
/// the typed query, with the typed text itself offered first.
pub struct SymbolDelegate {
    all: Vec<String>,
    shown: Vec<SymItem>,
}

impl SymbolDelegate {
    fn new(options: &[String], extras: &[String]) -> SymbolDelegate {
        let mut all = options.to_vec();
        for e in extras {
            if !all.contains(e) {
                all.push(e.clone());
            }
        }
        let shown = symbol_options::options_with_query("", &all).into_iter().map(SymItem).collect();
        SymbolDelegate { all, shown }
    }
}

impl SearchableListDelegate for SymbolDelegate {
    type Item = SymItem;

    fn items_count(&self, _: usize) -> usize {
        self.shown.len()
    }

    fn item(&self, ix: IndexPath) -> Option<&SymItem> {
        self.shown.get(ix.row)
    }

    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        SymItem: SearchableListItem<Value = V>,
        V: PartialEq,
    {
        self.shown.iter().position(|i| i.value() == value).map(|ix| IndexPath::default().row(ix))
    }

    fn perform_search(&mut self, query: &str, _: &mut Window, _: &mut App) -> Task<()> {
        self.shown = symbol_options::options_with_query(query, &self.all).into_iter().map(SymItem).collect();
        Task::ready(())
    }
}

/// Single-select: one symbol (trial symbol, manual order, cancel).
pub struct SymbolPicker {
    state: Entity<SelectState<SymbolDelegate>>,
    options: Vec<String>,
}

impl SymbolPicker {
    pub fn new(default: &str, window: &mut Window, cx: &mut Context<Shell>) -> SymbolPicker {
        let delegate = SymbolDelegate::new(&[], &[default.to_string()]);
        let state = cx.new(|cx| SelectState::new(delegate, Some(IndexPath::default().row(0)), window, cx).searchable(true));
        SymbolPicker { state, options: Vec::new() }
    }

    /// The selected symbol (empty = nothing chosen).
    pub fn value(&self, cx: &App) -> String {
        self.state.read(cx).selected_value().cloned().unwrap_or_default()
    }

    /// Replaces the candidates when they changed (so the open list is not rebuilt every frame).
    pub fn sync(&mut self, options: Vec<String>, window: &mut Window, cx: &mut Context<Shell>) {
        if options == self.options {
            return;
        }
        self.options = options;
        let cur = self.value(cx);
        self.apply(&cur, window, cx);
    }

    pub fn set_value(&mut self, value: &str, window: &mut Window, cx: &mut Context<Shell>) {
        self.apply(value.trim(), window, cx);
    }

    fn apply(&mut self, value: &str, window: &mut Window, cx: &mut Context<Shell>) {
        let extras: Vec<String> = if value.is_empty() { Vec::new() } else { vec![value.to_string()] };
        let delegate = SymbolDelegate::new(&self.options, &extras);
        let v = value.to_string();
        self.state.update(cx, |s, cx| {
            s.set_items(delegate, window, cx);
            if !v.is_empty() {
                s.set_selected_value(&v, window, cx);
            }
        });
    }

    /// True when a value is chosen and it is not in the candidate list (drives the hint).
    pub fn is_unlisted(&self, cx: &App) -> bool {
        let v = self.value(cx);
        !v.is_empty() && !symbol_options::is_known(&v, &self.options)
    }

    pub fn loaded(&self) -> bool {
        !self.options.is_empty()
    }

    pub fn element(&self, placeholder: &str) -> Select<SymbolDelegate> {
        Select::new(&self.state).placeholder(placeholder.to_string()).search_placeholder("搜尋或輸入 Symbol")
    }
}

/// Multi-select: `allowed_coins`.
pub struct CoinPicker {
    state: Entity<ComboboxState<SymbolDelegate>>,
    options: Vec<String>,
}

impl CoinPicker {
    pub fn new(window: &mut Window, cx: &mut Context<Shell>) -> CoinPicker {
        let state = cx.new(|cx| ComboboxState::new(SymbolDelegate::new(&[], &[]), Vec::new(), window, cx).multiple(true).searchable(true));
        CoinPicker { state, options: Vec::new() }
    }

    pub fn values(&self, cx: &App) -> Vec<String> {
        self.state.read(cx).selected_values()
    }

    /// The `allowed_coins` text (stored format).
    pub fn text(&self, cx: &App) -> String {
        symbol_options::join_coins(&self.values(cx))
    }

    pub fn sync(&mut self, options: Vec<String>, window: &mut Window, cx: &mut Context<Shell>) {
        if options == self.options {
            return;
        }
        self.options = options;
        let cur = self.values(cx);
        self.apply(&cur, window, cx);
    }

    /// Selects the coins of an `allowed_coins` text.
    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Shell>) {
        let coins = symbol_options::parse_coins(text);
        self.apply(&coins, window, cx);
    }

    fn apply(&mut self, coins: &[String], window: &mut Window, cx: &mut Context<Shell>) {
        let delegate = SymbolDelegate::new(&self.options, coins);
        let coins = coins.to_vec();
        self.state.update(cx, |s, cx| {
            s.set_items(delegate, window, cx);
            s.set_selected_values(&coins, window, cx);
        });
    }

    /// Selected coins that are not in the candidate list.
    pub fn unlisted(&self, cx: &App) -> Vec<String> {
        self.values(cx).into_iter().filter(|c| !symbol_options::is_known(c, &self.options)).collect()
    }

    pub fn loaded(&self) -> bool {
        !self.options.is_empty()
    }

    pub fn element(&self, placeholder: &str) -> Combobox<SymbolDelegate> {
        Combobox::new(&self.state).placeholder(placeholder.to_string()).search_placeholder("搜尋或輸入幣種")
    }
}

#[cfg(test)]
#[path = "symbol_select_tests.rs"]
mod tests;
