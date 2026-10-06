//! Task 2.5: "立即刷新" against fake transports (no network).
use std::sync::Arc;

use tong_funding_core::types::Exchange::{self, Binance, Bybit, Okx};

use super::*;
use crate::exchange::public::adapter::testkit::{ClockedTransport, block_on, fixture};
use crate::exchange::public::binance::BinanceAdapter;
use crate::exchange::public::bybit::BybitAdapter;
use crate::exchange::public::okx::OkxAdapter;
use crate::exchange::transport::{FakeTransport, HttpResponse};
use crate::ports::ManualClock;
use crate::ui::bridge::{UiSnapshot, apply_update};
use crate::ui::scanner::{self, TableState};
use crate::ui::testkit::complete_settings;

const NOW: i64 = 1_791_201_300_000;

fn ok(rel: &str) -> Result<HttpResponse, AdapterError> {
    Ok(HttpResponse::ok(fixture(rel)))
}

fn binance_fake() -> FakeTransport {
    FakeTransport::new()
        .on("/fapi/v1/premiumIndex", ok("binance/premiumIndex.json"))
        .on("/fapi/v1/ticker/24hr", ok("binance/ticker24hr.json"))
        .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
        .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"))
}

fn bybit_fake(fail_after_first: bool) -> FakeTransport {
    let t = FakeTransport::new().on("/v5/market/tickers", ok("bybit/tickers.json"));
    let t = if fail_after_first { t.on("/v5/market/tickers", Err(AdapterError::Timeout)) } else { t };
    t.on("/v5/market/instruments-info", ok("bybit/instruments_complete.json"))
}

fn okx_fake() -> FakeTransport {
    FakeTransport::new()
        .on("funding-rate?instId=ANY", ok("okx/funding_rate_any.json"))
        .on("mark-price?instType=SWAP", ok("okx/mark_price_swap.json"))
        .on("market/tickers?instType=SWAP", ok("okx/tickers_swap.json"))
        .on("public/instruments?instType=SWAP", ok("okx/instruments_swap.json"))
}

struct Rig {
    clock: ManualClock,
    tb: Arc<ClockedTransport>,
    ty: Arc<ClockedTransport>,
    to: Arc<ClockedTransport>,
    b: BinanceAdapter<ClockedTransport>,
    y: BybitAdapter<ClockedTransport>,
    o: OkxAdapter<ClockedTransport>,
}

fn rig(bybit_fails_on_refresh: bool, all_fail_on_refresh: bool) -> Rig {
    let clock = ManualClock::new(NOW);
    let mk = |f: FakeTransport| Arc::new(ClockedTransport::new(f, clock.clone(), 0));
    let fail = |f: FakeTransport, marker: &str| f.on(marker, Err(AdapterError::Timeout));
    let (fb, fo) = if all_fail_on_refresh {
        (fail(binance_fake(), "/fapi/v1/premiumIndex"), fail(okx_fake(), "funding-rate?instId=ANY"))
    } else {
        (binance_fake(), okx_fake())
    };
    let tb = mk(fb);
    let ty = mk(bybit_fake(bybit_fails_on_refresh || all_fail_on_refresh));
    let to = mk(fo);
    let b = BinanceAdapter::new(Arc::clone(&tb), Arc::new(clock.clone()));
    let y = BybitAdapter::new(Arc::clone(&ty), Arc::new(clock.clone()));
    let o = OkxAdapter::new(Arc::clone(&to), Arc::new(clock.clone()));
    Rig { clock, tb, ty, to, b, y, o }
}

fn enabled() -> BTreeSet<Exchange> {
    Exchange::ALL.into_iter().collect()
}

/// The regular poll that filled the table before the user clicked.
fn warm(r: &Rig) -> UiSnapshot {
    let mut s = UiSnapshot { settings: complete_settings("0.01"), ..Default::default() };
    let first = block_on(refresh_sources(&r.b, &r.y, &r.o, &enabled(), &r.clock));
    assert!(first.results.iter().all(|(_, x)| x.is_ok()), "{:?}", first.results.iter().map(|(e, x)| (e, x.as_ref().err())).collect::<Vec<_>>());
    for u in first.updates() {
        apply_update(&mut s, u);
    }
    s
}

fn market_requests(r: &Rig) -> [usize; 3] {
    [r.tb.count("/fapi/v1/premiumIndex"), r.ty.count("/v5/market/tickers"), r.to.count("funding-rate?instId=ANY")]
}

#[test]
fn refresh_sends_new_requests_to_every_source_and_recomputes_with_newer_data() {
    let r = rig(false, false);
    let mut snap = warm(&r);
    let before_counts = market_requests(&r);
    r.clock.advance(5_000);
    let clicked_at = NOW + 5_000;

    let gate = RefreshGate::default();
    let guard = gate.try_begin().expect("first click starts a refresh");
    let out = block_on(refresh_sources(&r.b, &r.y, &r.o, &enabled(), &r.clock));
    drop(guard);

    let after = market_requests(&r);
    for i in 0..3 {
        assert_eq!(after[i], before_counts[i] + 1, "source {i} got exactly one new market request");
    }
    for u in out.updates() {
        apply_update(&mut snap, u);
    }
    for e in Exchange::ALL {
        let feed = &snap.market[&e];
        assert!(!feed.observations.is_empty(), "{e:?}");
        assert!(feed.observations.iter().all(|o| o.observed_at >= clicked_at), "{e:?}: observed_at must be later than the click");
    }
    let vm = scanner::build(&snap, out.finished_at);
    assert_eq!(vm.computed_at, out.finished_at, "最新掃描 = this recompute");
    assert!(vm.computed_at >= clicked_at);
    assert!(!vm.rows.is_empty());
}

#[test]
fn a_second_click_while_refreshing_sends_nothing() {
    let r = rig(false, false);
    let _ = warm(&r);
    let gate = RefreshGate::default();
    let running = gate.try_begin().expect("first click");
    let before = r.tb.requests().len() + r.ty.requests().len() + r.to.requests().len();
    assert!(gate.try_begin().is_none(), "second click is ignored");
    assert!(gate.in_progress());
    let after = r.tb.requests().len() + r.ty.requests().len() + r.to.requests().len();
    assert_eq!(after, before, "no extra request");
    drop(running);
    assert!(!gate.in_progress());
    assert!(gate.try_begin().is_some(), "a later click works again");
}

#[test]
fn one_source_failing_updates_the_others_and_flags_the_failed_one() {
    let r = rig(true, false);
    let mut snap = warm(&r);
    r.clock.advance(5_000);
    let out = block_on(refresh_sources(&r.b, &r.y, &r.o, &enabled(), &r.clock));
    let failed: Vec<_> = out.results.iter().filter(|(_, x)| x.is_err()).map(|(e, _)| *e).collect();
    assert_eq!(failed, [Bybit]);
    for u in out.updates() {
        apply_update(&mut snap, u);
    }
    assert!(snap.market[&Binance].observations.iter().all(|o| o.observed_at >= NOW + 5_000));
    assert!(snap.market[&Okx].observations.iter().all(|o| o.observed_at >= NOW + 5_000));
    let bybit = &snap.market[&Bybit];
    assert!(bybit.failing());
    assert!(bybit.observations.iter().all(|o| o.observed_at == NOW), "old Bybit data is kept as old, not presented as new");
    let vm = scanner::build(&snap, out.finished_at);
    assert_eq!(vm.state, TableState::Ready);
    assert_eq!(vm.source_errors, vec![(Bybit, "request timed out".to_string())]);
    assert_eq!(vm.computed_at, out.finished_at, "最新掃描 still moves");
}

#[test]
fn all_sources_failing_keeps_the_old_table_and_says_quotes_are_unavailable() {
    let r = rig(false, true);
    let mut snap = warm(&r);
    let old_rows = scanner::build(&snap, NOW).rows.len();
    r.clock.advance(5_000);
    let out = block_on(refresh_sources(&r.b, &r.y, &r.o, &enabled(), &r.clock));
    assert!(out.all_failed());
    for u in out.updates() {
        apply_update(&mut snap, u);
    }
    let vm = scanner::build(&snap, NOW + 5_000);
    match &vm.state {
        TableState::Unavailable { errors } => assert_eq!(errors.len(), 3),
        other => panic!("{other:?}"),
    }
    assert_eq!(vm.rows.len(), old_rows, "old table kept");
    assert_eq!(vm.oldest_data_age_ms, Some(5_000), "old data carries its age");
}

#[test]
fn disabled_exchanges_are_not_requested() {
    let r = rig(false, false);
    let only: BTreeSet<_> = [Binance, Bybit].into();
    let out = block_on(refresh_sources(&r.b, &r.y, &r.o, &only, &r.clock));
    assert_eq!(out.results.iter().map(|(e, _)| *e).collect::<Vec<_>>(), [Binance, Bybit]);
    assert!(r.to.requests().is_empty());
}

// ---- trade-cost-estimate: top-of-book rides on the same refresh ----

#[test]
fn refresh_carries_best_bid_ask_for_every_source_and_adds_one_book_request_only_on_binance() {
    let r = rig(false, false);
    // Binance's bookTicker is scripted on top of the existing routes; unscripted = unavailable.
    let tb = Arc::new(ClockedTransport::new(binance_fake().on("/fapi/v1/ticker/bookTicker", ok("binance/bookTicker.json")), r.clock.clone(), 0));
    let b = BinanceAdapter::new(Arc::clone(&tb), Arc::new(r.clock.clone()));
    let out = block_on(refresh_sources(&b, &r.y, &r.o, &enabled(), &r.clock));
    assert_eq!(out.books.len(), 3);
    assert!(out.books.iter().all(|(_, x)| x.is_ok()), "{:?}", out.books);
    assert_eq!(tb.count("/fapi/v1/ticker/bookTicker"), 1);
    assert_eq!(r.ty.count("/v5/market/tickers"), 1, "Bybit: still ONE tickers request");
    assert_eq!(r.to.count("market/tickers?instType=SWAP"), 1, "OKX: still ONE tickers request");
    let mut snap = UiSnapshot::default();
    for u in out.updates() {
        apply_update(&mut snap, u);
    }
    for e in Exchange::ALL {
        let feed = &snap.books[&e];
        assert!(feed.books.contains_key("BTCUSDT"), "{e:?}");
        assert_eq!(feed.last_success_at, Some(out.finished_at));
        assert!(feed.last_error.is_none());
    }
}

#[test]
fn binance_book_failure_leaves_funding_observations_updated_and_flags_only_the_quotes() {
    let r = rig(false, false); // binance_fake() has no bookTicker route
    let out = block_on(refresh_sources(&r.b, &r.y, &r.o, &enabled(), &r.clock));
    assert!(out.results.iter().all(|(_, x)| x.is_ok()), "funding observations are fine");
    let bin = out.books.iter().find(|(e, _)| *e == Binance).unwrap();
    assert!(bin.1.is_err());
    let mut snap = UiSnapshot::default();
    for u in out.updates() {
        apply_update(&mut snap, u);
    }
    assert!(!snap.market[&Binance].observations.is_empty());
    assert!(snap.books[&Binance].books.is_empty());
    assert!(snap.books[&Binance].last_error.is_some());
    assert!(snap.books[&Bybit].books.contains_key("BTCUSDT"));
}
