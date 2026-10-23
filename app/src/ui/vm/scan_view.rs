//! Scanner table controls (spec scanner-table-controls): which columns are shown, and how the
//! rows are ordered when the user clicks a header. Everything here is pure; the table delegate
//! and the shell only wire it up. Sorting never changes `ScanRow.rank` (the default-order rank).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use tong_funding_core::types::{Decimal, Exchange};

use super::scanner::{Qualified, RateCell, RowsView, ScanRow, ScannerVm};

/// Font size of the table's cell text (`pages::small`), in px at 100% zoom.
pub const CELL_FONT_PX: f32 = 10.0;
/// Left plus right padding of a table cell.
pub const CELL_PAD_PX: f32 = 16.0;
/// Gap between the parts of one line in a cell (`gap_1`).
pub const CELL_GAP_PX: f32 = 4.0;
/// Row height at 100% zoom: two lines of cell text (line height 1.6) plus the cell's top and bottom padding.
pub const ROW_HEIGHT_PX: f32 = 44.0;

/// Width of `text` in one line, on the safe side: ASCII is as wide as a monospaced digit (0.6 em),
/// anything else (Chinese, full-width brackets, the minus sign) counts as a full em.
pub fn est_text_px(text: &str) -> f32 {
    text.chars().map(|c| if c.is_ascii() { 0.6 } else { 1.0 }).sum::<f32>() * CELL_FONT_PX
}

/// The columns of the Funding Rate Matrix, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScanColumn {
    Rank,
    Symbol,
    Coverage,
    Countdown,
    Binance,
    Bybit,
    Okx,
    Direction,
    Gross,
    NetEdge,
    Qualified,
    Add,
}

impl ScanColumn {
    pub const ALL: [ScanColumn; 12] = [
        ScanColumn::Rank,
        ScanColumn::Symbol,
        ScanColumn::Coverage,
        ScanColumn::Countdown,
        ScanColumn::Binance,
        ScanColumn::Bybit,
        ScanColumn::Okx,
        ScanColumn::Direction,
        ScanColumn::Gross,
        ScanColumn::NetEdge,
        ScanColumn::Qualified,
        ScanColumn::Add,
    ];

    /// The rate column of `ex`.
    pub fn for_exchange(ex: Exchange) -> ScanColumn {
        match ex {
            Exchange::Binance => ScanColumn::Binance,
            Exchange::Bybit => ScanColumn::Bybit,
            Exchange::Okx => ScanColumn::Okx,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            ScanColumn::Rank => "rank",
            ScanColumn::Symbol => "symbol",
            ScanColumn::Coverage => "cov",
            ScanColumn::Countdown => "cd",
            ScanColumn::Binance => "binance",
            ScanColumn::Bybit => "bybit",
            ScanColumn::Okx => "okx",
            ScanColumn::Direction => "dir",
            ScanColumn::Gross => "gross",
            ScanColumn::NetEdge => "net",
            ScanColumn::Qualified => "ok",
            ScanColumn::Add => "add",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            ScanColumn::Rank => "Rank",
            ScanColumn::Symbol => "Symbol",
            ScanColumn::Coverage => "覆蓋",
            ScanColumn::Countdown => "結算倒數",
            ScanColumn::Binance => "Binance",
            ScanColumn::Bybit => "Bybit",
            ScanColumn::Okx => "OKX",
            ScanColumn::Direction => "最佳套利方向",
            ScanColumn::Gross => "Gross Spread %",
            ScanColumn::NetEdge => "Net Edge %",
            ScanColumn::Qualified => "達標",
            ScanColumn::Add => "加入交易單",
        }
    }

    /// Width in px at 100% zoom. Each is the widest content the column draws plus the cell padding
    /// (`est_text_px`; the tests in `scan_view_tests` keep them in step with the texts), so nothing is
    /// cut off or runs into the next column (scanner-layout).
    pub fn width(self) -> f32 {
        match self {
            ScanColumn::Rank => 50.0,
            ScanColumn::Symbol => 120.0,
            ScanColumn::Coverage => 50.0,
            ScanColumn::Countdown => 120.0,
            ScanColumn::Binance | ScanColumn::Bybit | ScanColumn::Okx => 176.0,
            ScanColumn::Direction => 160.0,
            ScanColumn::Gross => 112.0,
            ScanColumn::NetEdge => 200.0,
            ScanColumn::Qualified => 80.0,
            ScanColumn::Add => 130.0,
        }
    }

    /// "最佳套利方向" and "加入交易單" cannot be sorted.
    pub fn sortable(self) -> bool {
        !matches!(self, ScanColumn::Direction | ScanColumn::Add)
    }

    /// Rank and Symbol stay on the left while the table scrolls sideways.
    pub fn fixed_left(self) -> bool {
        matches!(self, ScanColumn::Rank | ScanColumn::Symbol)
    }

    /// Symbol identifies the row, so it is always shown.
    pub fn hideable(self) -> bool {
        self != ScanColumn::Symbol
    }
}

/// Which columns are shown (session only, spec: not persisted). A column's visibility is the user's
/// explicit choice when there is one, otherwise a default: shown, except the rate column of an
/// exchange that is not in `allowed_exchanges` (scanner-layout).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnVisibility {
    /// What the user clicked: `true` = shown, `false` = hidden.
    choice: BTreeMap<ScanColumn, bool>,
    /// Columns hidden by default because their exchange is not enabled.
    disabled: BTreeSet<ScanColumn>,
}

impl ColumnVisibility {
    /// Tells which exchanges are not enabled (the shell calls this on every recompute). Only the
    /// default changes; the user's explicit choices stay.
    pub fn set_disabled_exchanges(&mut self, exchanges: &[Exchange]) {
        self.disabled = exchanges.iter().map(|e| ScanColumn::for_exchange(*e)).collect();
    }

    /// Returns whether anything changed (`Symbol` never changes).
    pub fn toggle(&mut self, col: ScanColumn) -> bool {
        if !col.hideable() {
            return false;
        }
        let now = self.is_visible(col);
        self.choice.insert(col, !now);
        true
    }

    /// Shows all twelve columns (spec scanner-table-controls), including a disabled exchange's.
    pub fn reset(&mut self) {
        self.choice.clear();
        self.choice.extend(self.disabled.iter().map(|c| (*c, true)));
    }

    pub fn is_visible(&self, col: ScanColumn) -> bool {
        !col.hideable() || self.choice.get(&col).copied().unwrap_or(!self.disabled.contains(&col))
    }

    pub fn is_all_visible(&self) -> bool {
        ScanColumn::ALL.iter().all(|c| self.is_visible(*c))
    }

    /// The shown columns in display order.
    pub fn visible(&self) -> Vec<ScanColumn> {
        ScanColumn::ALL.into_iter().filter(|c| self.is_visible(*c)).collect()
    }

    /// Sum of the widths of the shown columns.
    pub fn total_width(&self) -> f32 {
        self.visible().iter().map(|c| c.width()).sum()
    }

    /// The table scrolls sideways only when the shown columns are wider than the viewport.
    pub fn needs_horizontal_scroll(&self, viewport_width: f32) -> bool {
        self.total_width() > viewport_width
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortState {
    pub column: ScanColumn,
    pub dir: SortDir,
}

/// What the user did to the sort state (queued by the table delegate, applied by the shell).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanEvent {
    /// Header title clicked: descending, then ascending, then back to the default order.
    CycleSort(ScanColumn),
    /// The library's sort icon was clicked and it already cycled; `None` = default order.
    SetSort(ScanColumn, Option<SortDir>),
}

/// Everything the user can change on the scanner table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanViewState {
    pub visibility: ColumnVisibility,
    pub sort: Option<SortState>,
}

impl ScanViewState {
    /// Returns whether the state changed.
    pub fn apply(&mut self, ev: ScanEvent) -> bool {
        let before = self.sort;
        match ev {
            ScanEvent::CycleSort(col) if col.sortable() => {
                self.sort = match self.sort {
                    Some(SortState { column, dir: SortDir::Desc }) if column == col => Some(SortState { column: col, dir: SortDir::Asc }),
                    Some(SortState { column, dir: SortDir::Asc }) if column == col => None,
                    _ => Some(SortState { column: col, dir: SortDir::Desc }),
                };
            }
            ScanEvent::SetSort(col, dir) if col.sortable() => {
                self.sort = dir.map(|dir| SortState { column: col, dir });
            }
            _ => {}
        }
        self.sort != before
    }
}

/// The sort key of a numeric column; `None` = the row has no value there (always sorted last).
fn numeric_key(row: &ScanRow, col: ScanColumn) -> Option<Decimal> {
    let rate_of = |ex: Exchange| {
        row.cells.iter().find(|(e, _)| *e == ex).and_then(|(_, c)| match c {
            RateCell::Rate { rate, .. } => Some(*rate),
            _ => None,
        })
    };
    match col {
        ScanColumn::Rank => Some(Decimal::from(row.rank)),
        ScanColumn::Coverage => Some(Decimal::from(row.listed * 1_000 + row.enabled)),
        ScanColumn::Countdown => row.countdown_target.map(|(t, _)| Decimal::from(t)),
        ScanColumn::Binance => rate_of(Exchange::Binance),
        ScanColumn::Bybit => rate_of(Exchange::Bybit),
        ScanColumn::Okx => rate_of(Exchange::Okx),
        ScanColumn::Gross => row.gross_spread,
        ScanColumn::NetEdge => row.net_edge.value(),
        ScanColumn::Qualified => Some(Decimal::from(u8::from(row.qualified == Qualified::Yes))),
        ScanColumn::Symbol | ScanColumn::Direction | ScanColumn::Add => None,
    }
}

/// ASCII case-insensitive comparison without allocating.
fn cmp_ignore_case(a: &str, b: &str) -> Ordering {
    a.bytes().map(|c| c.to_ascii_lowercase()).cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

fn compare(a: &ScanRow, b: &ScanRow, sort: SortState) -> Ordering {
    let by_value = if sort.column == ScanColumn::Symbol {
        let o = cmp_ignore_case(&a.symbol, &b.symbol);
        if sort.dir == SortDir::Desc { o.reverse() } else { o }
    } else {
        match (numeric_key(a, sort.column), numeric_key(b, sort.column)) {
            // Rows without a value go last in both directions; among themselves they keep the default order.
            (None, None) => Ordering::Equal,
            (None, Some(_)) => return Ordering::Greater,
            (Some(_), None) => return Ordering::Less,
            (Some(x), Some(y)) => {
                let o = x.cmp(&y);
                if sort.dir == SortDir::Desc { o.reverse() } else { o }
            }
        }
    };
    by_value.then(a.rank.cmp(&b.rank))
}

/// Order `rows` by `sort`; `None` keeps the order given (the default order). Equal keys fall back
/// to the default rank, so the order is stable between refreshes.
pub fn sort_rows(rows: &mut [ScanRow], sort: Option<SortState>) {
    let Some(sort) = sort else { return };
    if !sort.column.sortable() {
        return;
    }
    rows.sort_by(|a, b| compare(a, b, sort));
}

/// The rows the table shows: the "只顯示達標" filter first, then the user's sort. `None` = the
/// toggle is on and nothing qualifies (the page shows its own message instead of a table).
pub fn table_rows(vm: &ScannerVm, only_qualified: bool, sort: Option<SortState>) -> Option<Vec<ScanRow>> {
    match vm.visible(only_qualified) {
        RowsView::Rows(rows) => {
            let mut rows: Vec<ScanRow> = rows.into_iter().cloned().collect();
            sort_rows(&mut rows, sort);
            Some(rows)
        }
        RowsView::NoneQualified => None,
    }
}

#[cfg(test)]
#[path = "scan_view_tests.rs"]
mod tests;
