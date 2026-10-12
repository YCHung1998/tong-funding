use super::*;
use crate::store::db::test_support::open_tmp;

const T: i64 = 1_791_302_400_000;

fn slot(pair: &str, leg: SlotLeg, ex: Exchange) -> SlotRow {
    SlotRow::new(pair, leg, ex, "NMRUSDT", T, T + 300_000, T + 10_000)
}

fn count_events(db: &Db, ty: &str) -> i64 {
    db.with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM events WHERE event_type = ?1", [ty], |r| r.get(0))?)).unwrap()
}

fn settled(mut s: SlotRow, key: &str, amount: &str) -> SlotRow {
    s.resolution = Some(Resolution::Ledger);
    s.amount = Some(amount.parse().unwrap());
    s.ledger_dedupe_key = Some(key.into());
    s.ledger_exchange_id = Some("1598827627629488510".into());
    s.first_seen_ms = Some(T + 206_000);
    s.checks_ok = 1;
    s.updated_ms = T + 306_000;
    s
}

#[test]
fn funding_slots_store_a_slot_is_unique_per_pair_leg_and_settlement() {
    let (_d, db, _) = open_tmp();
    let s = slot("p1", SlotLeg::Long, Exchange::Binance);
    assert!(db.insert_slot_if_absent(&s).unwrap());
    assert!(!db.insert_slot_if_absent(&s).unwrap(), "same key: no second row");
    assert!(db.insert_slot_if_absent(&slot("p1", SlotLeg::Short, Exchange::Bybit)).unwrap(), "other leg is another slot");
    let mut later = slot("p1", SlotLeg::Long, Exchange::Binance);
    later.settle_ms = T + 28_800_000;
    assert!(db.insert_slot_if_absent(&later).unwrap(), "next settlement is another slot");
    assert_eq!(db.slots_of_pair("p1").unwrap().len(), 3);
    assert_eq!(db.get_slot("p1", SlotLeg::Long, T).unwrap(), Some(s));
}

#[test]
fn funding_slots_store_survives_a_reopen_with_every_field() {
    let dir = tempfile::tempdir().unwrap();
    let clock: std::sync::Arc<dyn crate::ports::Clock> = std::sync::Arc::new(crate::ports::ManualClock::new(T));
    let path = dir.path().join("funding.db");
    let mut s = slot("p1", SlotLeg::Short, Exchange::Okx);
    s.state = SlotState::ManualCheck;
    s.manual_reason = Some(ManualReason::FetchFailed);
    s.last_error = Some("timeout".into());
    s.checks_failed = 4;
    {
        let db = Db::open(&path, clock.clone());
        assert!(db.insert_slot_if_absent(&s).unwrap());
    }
    let db = Db::open(&path, clock);
    assert_eq!(db.slots_of_pair("p1").unwrap(), vec![s]);
}

#[test]
fn funding_slots_store_due_slots_are_unsettled_and_past_their_check_time() {
    let (_d, db, _) = open_tmp();
    let a = slot("p1", SlotLeg::Long, Exchange::Binance); // due at T+5 min
    let mut b = slot("p2", SlotLeg::Long, Exchange::Binance);
    b.next_check_ms = T + 600_000;
    let mut c = slot("p3", SlotLeg::Long, Exchange::Binance);
    c.state = SlotState::ManualCheck;
    c.manual_reason = Some(ManualReason::NotFound);
    c.next_check_ms = T + 3_600_000;
    for s in [&a, &b, &c] {
        db.insert_slot_if_absent(s).unwrap();
    }
    assert!(db.due_slots(T + 299_999).unwrap().is_empty());
    assert_eq!(db.due_slots(T + 300_000).unwrap().iter().map(|s| s.pair_id.as_str()).collect::<Vec<_>>(), vec!["p1"]);
    assert_eq!(db.due_slots(T + 4_000_000).unwrap().len(), 3, "MANUAL_CHECK is due again at its sweep time");
    db.settle_slot(&settled(a, "binance:FUNDING_FEE:1", "11.5")).unwrap();
    assert_eq!(db.due_slots(T + 4_000_000).unwrap().len(), 2, "a settled slot is never due");
}

#[test]
fn funding_slots_store_settling_writes_one_linked_event_with_the_pair_id() {
    let (_d, db, _) = open_tmp();
    let s = slot("p1", SlotLeg::Long, Exchange::Binance);
    db.insert_slot_if_absent(&s).unwrap();
    assert!(db.settle_slot(&settled(s.clone(), "binance:FUNDING_FEE:1", "11.58677324")).unwrap());
    assert!(!db.settle_slot(&settled(s, "binance:FUNDING_FEE:1", "11.58677324")).unwrap(), "already settled: no second event");
    assert_eq!(count_events(&db, PAIR_FUNDING_LINKED), 1);
    let (pair, payload): (String, String) =
        db.with_conn(|c| Ok(c.query_row("SELECT pair_id, payload FROM events WHERE event_type = ?1", [PAIR_FUNDING_LINKED], |r| Ok((r.get(0)?, r.get(1)?)))?)).unwrap();
    assert_eq!(pair, "p1");
    let p: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!((p["leg"].as_str(), p["amount"].as_str(), p["delay_ms"].as_i64(), p["resolution"].as_str()), (Some("long"), Some("11.58677324"), Some(206_000), Some("LEDGER")));
    let stored = db.get_slot("p1", SlotLeg::Long, T).unwrap().unwrap();
    assert_eq!((stored.state, stored.amount), (SlotState::Settled, Some("11.58677324".parse().unwrap())));
}

#[test]
fn funding_slots_store_a_ledger_row_settles_at_most_one_slot() {
    let (_d, db, _) = open_tmp();
    let (a, b) = (slot("p1", SlotLeg::Long, Exchange::Binance), slot("p2", SlotLeg::Long, Exchange::Binance));
    db.insert_slot_if_absent(&a).unwrap();
    db.insert_slot_if_absent(&b).unwrap();
    assert!(db.settle_slot(&settled(a, "binance:FUNDING_FEE:1", "1")).unwrap());
    assert!(!db.settle_slot(&settled(b.clone(), "binance:FUNDING_FEE:1", "1")).unwrap(), "same ledger row for a second slot is refused");
    assert_eq!(count_events(&db, PAIR_FUNDING_LINKED), 1);
    assert_eq!(db.get_slot("p2", SlotLeg::Long, T).unwrap().unwrap().state, SlotState::Tracing1, "the refused slot stays unsettled");
}

#[test]
fn funding_slots_store_a_zero_rate_settlement_has_no_ledger_id() {
    let (_d, db, _) = open_tmp();
    let mut s = slot("p1", SlotLeg::Short, Exchange::Bybit);
    db.insert_slot_if_absent(&s).unwrap();
    s.resolution = Some(Resolution::ZeroRate);
    s.amount = Some(Decimal::ZERO);
    s.zero_rate_source = Some("bybit /v5/market/funding/history".into());
    s.first_seen_ms = Some(T + 300_000);
    assert!(db.settle_slot(&s).unwrap());
    let stored = db.get_slot("p1", SlotLeg::Short, T).unwrap().unwrap();
    assert_eq!((stored.resolution, stored.amount, stored.ledger_dedupe_key), (Some(Resolution::ZeroRate), Some(Decimal::ZERO), None));
    // Two zero-rate slots coexist (NULL keys are not unique).
    let mut other = slot("p2", SlotLeg::Short, Exchange::Bybit);
    db.insert_slot_if_absent(&other).unwrap();
    other.resolution = Some(Resolution::ZeroRate);
    other.amount = Some(Decimal::ZERO);
    assert!(db.settle_slot(&other).unwrap());
}

#[test]
fn funding_slots_store_progress_updates_never_touch_a_settled_slot() {
    let (_d, db, _) = open_tmp();
    let s = slot("p1", SlotLeg::Long, Exchange::Binance);
    db.insert_slot_if_absent(&s).unwrap();
    let mut p = s.clone();
    p.state = SlotState::Tracing2;
    p.checks_ok = 6;
    p.next_check_ms = T + 2_400_000;
    assert!(db.update_slot_progress(&p).unwrap());
    assert_eq!(db.get_slot("p1", SlotLeg::Long, T).unwrap().unwrap().state, SlotState::Tracing2);
    db.settle_slot(&settled(p.clone(), "binance:FUNDING_FEE:9", "2")).unwrap();
    p.state = SlotState::ManualCheck;
    assert!(!db.update_slot_progress(&p).unwrap(), "settled stays settled");
    assert_eq!(db.get_slot("p1", SlotLeg::Long, T).unwrap().unwrap().state, SlotState::Settled);
}

#[test]
fn funding_slots_store_invalid_states_are_refused_by_the_database() {
    let (_d, db, _) = open_tmp();
    let r = db.with_conn(|c| {
        Ok(c.execute(
            "INSERT INTO funding_slots (pair_id, leg, exchange, symbol, settle_ms, state, next_check_ms, created_ms, updated_ms) VALUES ('p','long','Binance','X',1,'WHATEVER',1,1,1)",
            [],
        )?)
    });
    assert!(r.is_err());
}

#[test]
fn funding_slots_store_a_recheck_makes_only_the_unsettled_slots_of_that_pair_due_now() {
    let (_d, db, _) = open_tmp();
    let mut manual = slot("p1", SlotLeg::Long, Exchange::Binance);
    manual.state = SlotState::ManualCheck;
    manual.manual_reason = Some(ManualReason::NotFound);
    manual.next_check_ms = T + 3_600_000;
    let done = slot("p1", SlotLeg::Short, Exchange::Bybit);
    let other = slot("p2", SlotLeg::Long, Exchange::Binance);
    for s in [&manual, &done, &other] {
        db.insert_slot_if_absent(s).unwrap();
    }
    db.settle_slot(&settled(done, "bybit:1", "0.36")).unwrap();
    let now = T + 100_000;
    assert_eq!(db.request_slot_recheck("p1", now).unwrap(), 1, "only the unsettled slot of p1");
    let m = db.get_slot("p1", SlotLeg::Long, T).unwrap().unwrap();
    assert_eq!((m.next_check_ms, m.state, m.manual_reason), (now, SlotState::ManualCheck, Some(ManualReason::NotFound)));
    assert_eq!(db.get_slot("p2", SlotLeg::Long, T).unwrap().unwrap().next_check_ms, T + 300_000, "another pair is untouched");
    assert_eq!(db.due_slots(now).unwrap().iter().map(|s| s.pair_id.as_str()).collect::<Vec<_>>(), vec!["p1"]);
}

#[test]
fn funding_slots_store_v4_keeps_the_v3_rows_and_accepts_no_settlement() {
    use crate::store::db::{MIGRATIONS, Migration};
    use crate::store::schema::{SCHEMA_V1, SCHEMA_V2, SCHEMA_V3};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("funding.db");
    let clock: std::sync::Arc<dyn crate::ports::Clock> = std::sync::Arc::new(crate::ports::ManualClock::new(T));
    let v3 = [Migration { version: 1, sql: SCHEMA_V1 }, Migration { version: 2, sql: SCHEMA_V2 }, Migration { version: 3, sql: SCHEMA_V3 }];
    let mut manual = slot("p1", SlotLeg::Long, Exchange::Binance);
    manual.state = SlotState::ManualCheck;
    manual.manual_reason = Some(ManualReason::NotFound);
    manual.checks_ok = 9;
    let done = settled(slot("p1", SlotLeg::Short, Exchange::Bybit), "bybit:1", "-1.6997898");
    {
        let db = Db::open_with(&path, clock.clone(), &v3);
        assert!(!db.is_halted(), "{:?}", db.halt_reason());
        db.insert_slot_if_absent(&manual).unwrap();
        db.insert_slot_if_absent(&slot("p1", SlotLeg::Short, Exchange::Bybit)).unwrap();
        db.settle_slot(&done).unwrap();
        // v3 cannot hold NO_SETTLEMENT.
        assert!(db.with_conn(|c| Ok(c.execute("UPDATE funding_slots SET resolution = 'NO_SETTLEMENT' WHERE leg = 'long'", [])?)).is_err());
    }
    let db = Db::open(&path, clock);
    assert!(!db.is_halted(), "{:?}", db.halt_reason());
    assert_eq!(MIGRATIONS.last().unwrap().version, 4);
    let rows = db.slots_of_pair("p1").unwrap();
    assert_eq!(rows.len(), 2, "both rows survive the rebuild");
    assert_eq!(db.get_slot("p1", SlotLeg::Long, T).unwrap().unwrap(), manual);
    let kept = db.get_slot("p1", SlotLeg::Short, T).unwrap().unwrap();
    assert_eq!((kept.state, kept.amount, kept.ledger_dedupe_key.as_deref()), (SlotState::Settled, Some("-1.6997898".parse().unwrap()), Some("bybit:1")));
    // The unique ledger key, the due index and the new resolution all work after the rebuild.
    let mut n = manual.clone();
    n.resolution = Some(Resolution::NoSettlement);
    n.amount = Some(Decimal::ZERO);
    n.first_seen_ms = Some(T + 4_000_000);
    assert!(db.settle_slot(&n).unwrap());
    assert_eq!(db.get_slot("p1", SlotLeg::Long, T).unwrap().unwrap().resolution, Some(Resolution::NoSettlement));
    assert!(db.due_slots(T + 10_000_000).unwrap().is_empty());
}
