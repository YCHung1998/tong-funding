//! Scanner table controls (spec scanner-table-controls): which columns are shown, and how the
//! rows are ordered when the user clicks a header. Everything here is pure; the table delegate
//! and the shell only wire it up. Sorting never changes `ScanRow.rank` (the default-order rank).

use std::cmp::Ordering;
use std::collections::BTreeSet;

use tong_funding_core::types::{Decimal, Exchange};

use super::scanner::{Qualified, RateCell, RowsView, ScanRow, ScannerVm};

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

    pub fn width(self) -> f32 {
        match self {
            ScanColumn::Rank => 50.0,
            ScanColumn::Symbol => 120.0,
            ScanColumn::Coverage => 50.0,
            ScanColumn::Countdown => 110.0,
            ScanColumn::Binance | ScanColumn::Bybit | ScanColumn::Okx => 150.0,
            ScanColumn::Direction => 160.0,
            ScanColumn::Gross => 120.0,
            ScanColumn::NetEdge => 200.0,
            ScanColumn::Qualified => 60.0,
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

/// Which columns the user hid (session only, spec: not persisted).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnVisibility {
    hidden: BTreeSet<ScanColumn>,
}

impl ColumnVisibility {
    /// Returns whether anything changed (`Symbol` never changes).
    pub fn toggle(&mut self, col: ScanColumn) -> bool {
        if !col.hideable() {
            return false;
        }
        if !self.hidden.remove(&col) {
            self.hidden.insert(col);
        }
        true
    }

    pub fn reset(&mut self) {
        self.hidden.clear();
    }

    pub fn is_visible(&self, col: ScanColumn) -> bool {
        !self.hidden.contains(&col)
    }

    pub fn is_all_visible(&self) -> bool {
        self.hidden.is_empty()
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
