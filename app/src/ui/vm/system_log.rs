//! System log (系統日誌) view-model (task 2.3, spec system-log-page). Merges one page of the
//! permanent `events` table with this run's in-memory `SCAN_RUN` buffer, newest first. Read-only:
//! the page offers filters and "load older" only.

use std::collections::{BTreeMap, BTreeSet};

use crate::store::event_query::{EventPage, EventQuery, PAGE_SIZE, StoredEvent};
use crate::store::events::SCAN_RUN;
use crate::store::scan_buffer::ScanRecord;

use super::format;

pub const SCAN_RUN_NOTE: &str = "重啟後不保留";
pub const SCAN_RUN_TAG: &str = "僅本次運行";
pub const IMPORTED_TAG: &str = "匯入";
pub const MALFORMED_TAG: &str = "格式錯誤";

/// Where a timeline row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Stored { id: i64, imported: bool },
    /// The memory `SCAN_RUN` buffer of this run.
    Buffer,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogRow {
    pub ts_ms: i64,
    pub event_type: String,
    pub pair_id: Option<String>,
    /// Full payload, pretty-printed (no field removed or renamed); raw text if not JSON.
    pub detail: String,
    pub tags: Vec<&'static str>,
    pub origin: Origin,
}

impl LogRow {
    pub fn time_text(&self) -> String {
        format::utc_ms(self.ts_ms)
    }
}

/// Type filter: `None` = every type (default).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogFilter {
    pub types: Option<BTreeSet<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Timeline {
    Rows(Vec<LogRow>),
    NothingSelected,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SystemLogVm {
    /// Options from the data (distinct stored types + `SCAN_RUN` when buffered), by name.
    pub type_options: Vec<String>,
    /// Matching rows overall (not just loaded), and their range.
    pub total: i64,
    /// `最早 — 最新 UTC`.
    pub range_text: Option<String>,
    pub timeline: Timeline,
    /// The query for "load older" (`None` = nothing older).
    pub older: Option<EventQuery>,
    /// `ORDER_SUBMITTED 5 · SCAN_RUN 2 · FETCH_ERROR 1` (each type counted separately).
    pub footer: String,
    /// When the buffer is full: the time its oldest kept record starts at.
    pub buffer_note: Option<String>,
}

/// The first-page query for a filter. The `SCAN_RUN` type is answered from memory, so it is
/// removed from the database filter when it is the only thing that would match.
pub fn first_query(filter: &LogFilter) -> EventQuery {
    EventQuery { types: filter.types.clone(), before: None, limit: PAGE_SIZE }
}

/// Builds the timeline from one database page (already filtered by `filter`) and the buffer.
/// `loaded` holds rows from earlier pages (for "load older"); the new rows are appended.
pub fn build(page: &EventPage, buffer: &[ScanRecord], buffer_capacity: usize, filter: &LogFilter, query: &EventQuery, loaded: &[LogRow]) -> SystemLogVm {
    let selected = |t: &str| filter.types.as_ref().is_none_or(|s| s.contains(t));
    let scan_selected = selected(SCAN_RUN);

    // Buffer rows belong to this page when they fall inside its time window: newer than the
    // cursor (if any) and not older than the page's oldest row (unless nothing older exists).
    let upper = query.before.map(|(ts, _)| ts);
    let lower = if page.has_more { page.rows.last().map(|r| r.ts_ms) } else { None };
    let in_window = |ts: i64| upper.is_none_or(|u| ts < u) && lower.is_none_or(|l| ts >= l);

    let mut fresh: Vec<LogRow> = page.rows.iter().map(stored_row).collect();
    if scan_selected {
        fresh.extend(buffer.iter().filter(|r| in_window(r.ts_ms)).map(buffer_row));
    }
    fresh.sort_by(|a, b| b.ts_ms.cmp(&a.ts_ms).then_with(|| order_key(b).cmp(&order_key(a))));
    let mut rows = loaded.to_vec();
    rows.extend(fresh);

    let buffered = if scan_selected { buffer.len() as i64 } else { 0 };
    let total = page.matching + buffered;
    let buf_min = buffer.iter().map(|r| r.ts_ms).min().filter(|_| scan_selected);
    let buf_max = buffer.iter().map(|r| r.ts_ms).max().filter(|_| scan_selected);
    let oldest = [page.oldest_ts, buf_min].into_iter().flatten().min();
    let newest = [page.newest_ts, buf_max].into_iter().flatten().max();
    let range_text = oldest.zip(newest).map(|(o, n)| format!("{} — {} UTC", format::utc_ms(o), format::utc_ms(n)));

    let mut counts: BTreeMap<String, i64> = page.type_counts.iter().cloned().collect();
    if !buffer.is_empty() {
        *counts.entry(SCAN_RUN.to_string()).or_default() += buffer.len() as i64;
    }
    let type_options: Vec<String> = counts.keys().cloned().collect();
    let mut by_count: Vec<(&String, &i64)> = counts.iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let footer = by_count.iter().map(|(t, n)| format!("{t} {n}")).collect::<Vec<_>>().join(" · ");

    let older = page.has_more.then(|| page.rows.last().map(|r| EventQuery { types: query.types.clone(), before: Some((r.ts_ms, r.id)), limit: query.limit })).flatten();
    let buffer_note = (buffer_capacity > 0 && buffer.len() >= buffer_capacity)
        .then(|| buffer.iter().map(|r| r.ts_ms).min())
        .flatten()
        .map(|start| format!("SCAN_RUN 僅保留本次運行最近 {buffer_capacity} 筆，自 {} UTC 起（{SCAN_RUN_NOTE}）", format::utc_ms(start)));

    let nothing = filter.types.as_ref().is_some_and(|s| s.is_empty());
    SystemLogVm {
        type_options,
        total: if nothing { 0 } else { total },
        range_text: if nothing { None } else { range_text },
        timeline: if nothing { Timeline::NothingSelected } else { Timeline::Rows(rows) },
        older: if nothing { None } else { older },
        footer,
        buffer_note,
    }
}

/// Tie-break for equal timestamps: stored rows by id, buffer rows first among equals.
fn order_key(r: &LogRow) -> i64 {
    match r.origin {
        Origin::Stored { id, .. } => id,
        Origin::Buffer => i64::MAX,
    }
}

fn stored_row(e: &StoredEvent) -> LogRow {
    let (detail, malformed) = detail_of(&e.payload);
    let mut tags = Vec::new();
    if e.imported {
        tags.push(IMPORTED_TAG);
    }
    if malformed {
        tags.push(MALFORMED_TAG);
    }
    LogRow { ts_ms: e.ts_ms, event_type: e.event_type.clone(), pair_id: e.pair_id.clone(), detail, tags, origin: Origin::Stored { id: e.id, imported: e.imported } }
}

fn buffer_row(r: &ScanRecord) -> LogRow {
    let detail = serde_json::to_string_pretty(&r.payload).unwrap_or_else(|_| r.payload.to_string());
    LogRow { ts_ms: r.ts_ms, event_type: SCAN_RUN.to_string(), pair_id: r.pair_id.clone(), detail, tags: vec![SCAN_RUN_TAG], origin: Origin::Buffer }
}

/// Pretty JSON of a stored payload, or the raw text plus whether it was malformed.
pub fn detail_of(raw: &str) -> (String, bool) {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => (serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw.to_string()), false),
        Err(_) => (raw.to_string(), true),
    }
}

#[cfg(test)]
#[path = "system_log_tests.rs"]
mod tests;
