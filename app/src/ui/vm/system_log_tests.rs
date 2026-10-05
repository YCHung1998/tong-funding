//! Task 2.3: system log view-model (spec system-log-page).
use serde_json::json;

use super::*;
use crate::store::db::Db;
use crate::store::db::test_support::open_tmp;
use crate::store::events::EventStore;
use crate::store::scan_buffer::ScanRecord;

const DAY: i64 = 1_791_158_400_000; // 2026-10-05 00:00:00 UTC

fn at(h: i64, m: i64, s: i64) -> i64 {
    DAY + ((h * 60 + m) * 60 + s) * 1000
}

fn page_for(db: &Db, filter: &LogFilter) -> (EventQuery, EventPage) {
    let q = first_query(filter);
    let p = db.query_events(&q).unwrap();
    (q, p)
}

fn rows(vm: &SystemLogVm) -> &Vec<LogRow> {
    match &vm.timeline {
        Timeline::Rows(r) => r,
        other => panic!("{other:?}"),
    }
}

#[test]
fn newest_first_with_date_and_milliseconds() {
    let (_d, db, clock) = open_tmp();
    let es = EventStore::new(db.clone());
    clock.set(at(7, 41, 58));
    es.append("ORDER_SUBMITTED", None, json!({"n": 1})).unwrap();
    clock.set(at(7, 42, 16));
    es.append("ORDER_SUBMITTED", None, json!({"n": 2})).unwrap();
    let f = LogFilter::default();
    let (q, p) = page_for(&db, &f);
    let vm = build(&p, &[], 200, &f, &q, &[]);
    let r = rows(&vm);
    assert_eq!(r[0].time_text(), "2026-10-05 07:42:16.000");
    assert_eq!(r[1].time_text(), "2026-10-05 07:41:58.000");
    assert_eq!(vm.total, 2);
    assert_eq!(vm.range_text.as_deref(), Some("2026-10-05 07:41:58.000 — 2026-10-05 07:42:16.000 UTC"));
}

#[test]
fn seven_thousand_events_load_one_page_first_then_older_pages() {
    let (_d, db, clock) = open_tmp();
    let es = EventStore::new(db.clone());
    for i in 0..7_000 {
        clock.set(DAY + i);
        es.append(if i % 2 == 0 { "FETCH_ERROR" } else { "ORDER_SUBMITTED" }, None, json!({ "i": i })).unwrap();
    }
    let f = LogFilter::default();
    let (q, p) = page_for(&db, &f);
    let vm = build(&p, &[], 200, &f, &q, &[]);
    assert_eq!(rows(&vm).len(), PAGE_SIZE, "only the newest page is loaded");
    assert_eq!(PAGE_SIZE, 500);
    assert_eq!(vm.total, 7_000, "header shows every matching event");
    let older = vm.older.clone().expect("a way to load older events");
    let p2 = db.query_events(&older).unwrap();
    let vm2 = build(&p2, &[], 200, &f, &older, rows(&vm));
    let all = rows(&vm2);
    assert_eq!(all.len(), 2 * PAGE_SIZE);
    assert!(all.windows(2).all(|w| w[0].ts_ms > w[1].ts_ms), "strictly newest first, no overlap");
}

#[test]
fn scan_run_buffer_rows_merge_by_time_and_are_tagged() {
    let (_d, db, clock) = open_tmp();
    let es = EventStore::new(db.clone());
    clock.set(at(7, 0, 0));
    es.append("ORDER_SUBMITTED", None, json!({})).unwrap();
    clock.set(at(7, 0, 2));
    es.append("FETCH_ERROR", None, json!({})).unwrap();
    let buffer = vec![ScanRecord { ts_ms: at(7, 0, 1), pair_id: None, payload: json!({"rows": 528}) }];
    let f = LogFilter::default();
    let (q, p) = page_for(&db, &f);
    let vm = build(&p, &buffer, 200, &f, &q, &[]);
    let types: Vec<_> = rows(&vm).iter().map(|r| r.event_type.as_str()).collect();
    assert_eq!(types, ["FETCH_ERROR", "SCAN_RUN", "ORDER_SUBMITTED"]);
    let scan = &rows(&vm)[1];
    assert!(scan.tags.contains(&SCAN_RUN_TAG));
    assert_eq!(scan.origin, Origin::Buffer);
    assert!(vm.type_options.contains(&"SCAN_RUN".to_string()));
    assert_eq!(vm.total, 3);
}

#[test]
fn after_a_restart_scan_runs_are_gone_but_stored_events_remain() {
    let dir = crate::store::db::test_support::tempdir();
    let path = dir.path().join("funding.db");
    {
        let (c, _) = crate::store::db::test_support::clock(at(1, 0, 0));
        let es = EventStore::new(Db::open(&path, c));
        es.append("SCAN_RUN", None, json!({})).unwrap();
        es.append("ORDER_SUBMITTED", None, json!({})).unwrap();
        assert_eq!(es.scan_runs().len(), 1);
    }
    let (c, _) = crate::store::db::test_support::clock(at(2, 0, 0));
    let es = EventStore::new(Db::open(&path, c));
    let f = LogFilter::default();
    let (q, p) = page_for(es.db(), &f);
    let vm = build(&p, &es.scan_runs(), 200, &f, &q, &[]);
    let types: Vec<_> = rows(&vm).iter().map(|r| r.event_type.as_str()).collect();
    assert_eq!(types, ["ORDER_SUBMITTED"]);
    assert!(!vm.type_options.contains(&"SCAN_RUN".to_string()));
}

#[test]
fn imported_events_are_tagged() {
    let (_d, db, _) = open_tmp();
    db.with_conn(|c| Ok(c.execute("INSERT INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (5, 'ORDER_SUBMITTED', '{}', 'abc')", [])?)).unwrap();
    let f = LogFilter::default();
    let (q, p) = page_for(&db, &f);
    let vm = build(&p, &[], 200, &f, &q, &[]);
    assert!(rows(&vm)[0].tags.contains(&IMPORTED_TAG));
}

fn mixed(db: &Db, clock: &crate::ports::ManualClock) {
    let es = EventStore::new(db.clone());
    for (i, ty) in ["ORDER_SUBMITTED", "ORDER_SUBMITTED", "ORDER_SUBMITTED", "ORDER_SUBMITTED", "ORDER_SUBMITTED", "FETCH_ERROR"].iter().enumerate() {
        clock.set(DAY + i as i64 * 1000);
        es.append(ty, None, json!({})).unwrap();
    }
}

#[test]
fn filtering_one_type_updates_rows_and_total_and_footer_counts_each_type() {
    let (_d, db, clock) = open_tmp();
    mixed(&db, &clock);
    let buffer = vec![ScanRecord { ts_ms: DAY + 500, pair_id: None, payload: json!({}) }, ScanRecord { ts_ms: DAY + 1500, pair_id: None, payload: json!({}) }];
    let f = LogFilter { types: Some(["FETCH_ERROR".to_string()].into()) };
    let (q, p) = page_for(&db, &f);
    let vm = build(&p, &buffer, 200, &f, &q, &[]);
    assert!(rows(&vm).iter().all(|r| r.event_type == "FETCH_ERROR"));
    assert_eq!(vm.total, 1);
    assert_eq!(vm.footer, "ORDER_SUBMITTED 5 · SCAN_RUN 2 · FETCH_ERROR 1");
}

#[test]
fn a_new_type_in_the_table_becomes_an_option_automatically() {
    let (_d, db, clock) = open_tmp();
    mixed(&db, &clock);
    EventStore::new(db.clone()).append("FEED_RECOVERED", None, json!({"source": "bybit"})).unwrap();
    let f = LogFilter::default();
    let (q, p) = page_for(&db, &f);
    let vm = build(&p, &[], 200, &f, &q, &[]);
    assert_eq!(vm.type_options, ["FEED_RECOVERED", "FETCH_ERROR", "ORDER_SUBMITTED"]);
}

#[test]
fn unticking_every_type_says_nothing_is_selected() {
    let (_d, db, clock) = open_tmp();
    mixed(&db, &clock);
    let f = LogFilter { types: Some(BTreeSet::new()) };
    let (q, p) = page_for(&db, &f);
    let vm = build(&p, &[ScanRecord { ts_ms: 1, pair_id: None, payload: json!({}) }], 200, &f, &q, &[]);
    assert_eq!(vm.timeline, Timeline::NothingSelected);
}

#[test]
fn detail_contains_every_field_and_redacted_values_as_is() {
    let (_d, db, _) = open_tmp();
    let payload = json!({
        "a1": 1, "a2": "x", "a3": true, "a4": null, "a5": [1, 2], "a6": {"k": "v"},
        "a7": "0.019", "a8": -3, "a9": "BTCUSDT", "a10": "Binance", "a11": "LONG", "api_key": "RAW"
    });
    EventStore::new(db.clone()).append("ORDER_SUBMITTED", None, payload).unwrap();
    let f = LogFilter::default();
    let (q, p) = page_for(&db, &f);
    let vm = build(&p, &[], 200, &f, &q, &[]);
    let detail = &rows(&vm)[0].detail;
    let back: serde_json::Value = serde_json::from_str(detail).unwrap();
    assert_eq!(back.as_object().unwrap().len(), 12, "{detail}");
    assert!(!detail.contains("RAW"), "stored redacted");
    assert_eq!(back["api_key"], p.rows[0].payload.parse::<serde_json::Value>().unwrap()["api_key"], "placeholder shown as stored");
}

#[test]
fn malformed_payload_is_shown_raw_and_flagged() {
    let (text, bad) = detail_of("not json {");
    assert_eq!((text.as_str(), bad), ("not json {", true));
    let (text, bad) = detail_of(r#"{"b":1,"a":2}"#);
    assert!(!bad);
    assert!(text.contains("\"a\": 2") && text.contains("\"b\": 1"));
}

#[test]
fn fetch_error_and_feed_recovered_stay_two_rows() {
    let (_d, db, clock) = open_tmp();
    let es = EventStore::new(db.clone());
    clock.set(at(3, 0, 0));
    es.append("FETCH_ERROR", None, json!({"source": "bybit"})).unwrap();
    clock.set(at(3, 0, 30));
    es.append("FEED_RECOVERED", None, json!({"source": "bybit"})).unwrap();
    let f = LogFilter::default();
    let (q, p) = page_for(&db, &f);
    let vm = build(&p, &[], 200, &f, &q, &[]);
    let types: Vec<_> = rows(&vm).iter().map(|r| r.event_type.as_str()).collect();
    assert_eq!(types, ["FEED_RECOVERED", "FETCH_ERROR"]);
    assert!(vm.footer.contains("FEED_RECOVERED 1") && vm.footer.contains("FETCH_ERROR 1"));
}

#[test]
fn a_full_buffer_shows_where_its_coverage_starts() {
    let buffer: Vec<_> = (0..3).map(|i| ScanRecord { ts_ms: at(5, 0, i), pair_id: None, payload: json!({}) }).collect();
    let page = EventPage::default();
    let f = LogFilter::default();
    let vm = build(&page, &buffer, 3, &f, &first_query(&f), &[]);
    assert_eq!(vm.buffer_note.as_deref(), Some("SCAN_RUN 僅保留本次運行最近 3 筆，自 2026-10-05 05:00:00.000 UTC 起（重啟後不保留）"));
    let vm = build(&page, &buffer, 200, &f, &first_query(&f), &[]);
    assert_eq!(vm.buffer_note, None);
}
