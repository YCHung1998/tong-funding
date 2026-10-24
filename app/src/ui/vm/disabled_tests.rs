//! The one rule for an unticked exchange (ui-plain-language review fix).
use tong_funding_core::pair::PairState;
use tong_funding_core::types::Exchange::{Binance, Bybit, Okx};

use super::*;
use crate::ui::bridge::{PairInfo, PairRecord};
use crate::ui::testkit::{loaded, pair, position};

const NOW: i64 = 1_791_201_600_000;

fn record(id: &str, long: Option<Exchange>, short: Option<Exchange>, status: &str) -> (String, PairRecord) {
    (id.into(), PairRecord { pair_id: id.into(), symbol: "SOLUSDT".into(), long_exchange: long, short_exchange: short, status: status.into() })
}

#[test]
fn a_successful_empty_read_is_quiet() {
    let mut s = UiSnapshot::default();
    s.accounts.insert(Okx, loaded(vec![], vec![], NOW));
    assert_eq!(status(&s, Okx), DisabledStatus::Quiet);
}

#[test]
fn positions_an_incomplete_list_or_an_unfinished_pair_are_exposure() {
    let mut s = UiSnapshot::default();
    s.accounts.insert(Okx, loaded(vec![], vec![position(Okx, "ETHUSDT", "-0.07", "1500", "1500", "3", "0", None)], NOW));
    assert_eq!(status(&s, Okx), DisabledStatus::Exposed);
    let mut okx = loaded(vec![], vec![], NOW);
    if let AccountState::Loaded { data, .. } = &mut okx {
        data.positions_incomplete = Some("ctVal 未知".into());
    }
    s.accounts.insert(Okx, okx);
    assert_eq!(status(&s, Okx), DisabledStatus::Exposed);
    let mut s = UiSnapshot::default();
    s.accounts.insert(Okx, AccountState::Failed { error: "x".into(), at: NOW });
    s.pairs = vec![pair("p1", "SOLUSDT", Binance, Okx, PairState::Closing)];
    assert_eq!(status(&s, Okx), DisabledStatus::Exposed);
}

#[test]
fn every_state_but_the_terminal_ones_is_unfinished_and_an_unreadable_one_counts() {
    for st in PairState::ALL {
        let s = UiSnapshot { pairs: vec![pair("p1", "SOLUSDT", Binance, Okx, st)], ..Default::default() };
        assert_eq!(unfinished_pair_symbols(&s, Okx).len(), usize::from(!st.is_terminal()), "engine pair {st:?}");
        let s = UiSnapshot { pair_records: [record("u1", Some(Okx), Some(Bybit), st.as_str())].into(), ..Default::default() };
        assert_eq!(unfinished_pair_symbols(&s, Okx).len(), usize::from(!st.is_terminal()), "stored record {st:?}");
    }
    assert!(PairState::ALL.iter().filter(|s| s.is_terminal()).count() == 3, "FINALIZED, CANCELLED, BLOCKED");
    let unreadable = PairInfo { pair_id: "p1".into(), symbol: "SOLUSDT".into(), long_exchange: Binance, short_exchange: Okx, state: Err("???".into()) };
    let s = UiSnapshot { pairs: vec![unreadable], ..Default::default() };
    assert_eq!(unfinished_pair_symbols(&s, Okx), ["SOLUSDT"]);
    let s = UiSnapshot { pair_records: [record("u1", Some(Okx), Some(Bybit), "SOMETHING_NEW")].into(), ..Default::default() };
    assert_eq!(unfinished_pair_symbols(&s, Okx), ["SOLUSDT"], "an unknown stored status is unfinished");
}

#[test]
fn a_pair_is_counted_once_and_only_for_the_exchange_it_is_on() {
    let s = UiSnapshot {
        pairs: vec![pair("p1", "SOLUSDT", Binance, Okx, PairState::Reconciled)],
        pair_records: [record("p1", Some(Binance), Some(Okx), "RECONCILED")].into(),
        ..Default::default()
    };
    assert_eq!(unfinished_pair_symbols(&s, Okx), ["SOLUSDT"], "the engine pair and its stored record are one pair");
    assert!(unfinished_pair_symbols(&s, Bybit).is_empty());
}

#[test]
fn a_stored_record_with_no_exchange_could_be_on_any_of_them() {
    let s = UiSnapshot { pair_records: [record("u1", None, None, "RECONCILED")].into(), ..Default::default() };
    assert_eq!(unfinished_pair_symbols(&s, Okx), ["SOLUSDT"]);
    let s = UiSnapshot { pair_records: [record("u1", None, None, "FINALIZED")].into(), ..Default::default() };
    assert!(unfinished_pair_symbols(&s, Okx).is_empty());
}

#[test]
fn a_read_that_is_not_good_is_unconfirmed_with_a_short_reason() {
    let mut s = UiSnapshot::default();
    let reason = |s: &UiSnapshot| match status(s, Okx) {
        DisabledStatus::Unconfirmed { reason } => reason,
        other => panic!("{other:?}"),
    };
    assert_eq!(reason(&s), "載入中", "no read yet");
    s.accounts.insert(Okx, AccountState::NotConnected { reason: "Keychain 沒有 API key".into() });
    assert_eq!(reason(&s), "未連線：Keychain 沒有 API key");
    s.accounts.insert(Okx, AccountState::Failed { error: "exchange error acct_mode_unsupported: x".into(), at: NOW });
    assert_eq!(reason(&s), "OKX 帳戶模式不支援（在 OKX 改為單幣種或跨幣種保證金模式）");
    s.accounts.insert(Okx, AccountState::Failed { error: "x".repeat(200), at: NOW });
    assert_eq!(reason(&s).chars().count(), REASON_CHARS + 1, "long messages are cut on the visible line");
    let mut stale = loaded(vec![], vec![], NOW - 60_000);
    if let AccountState::Loaded { error, .. } = &mut stale {
        *error = Some(("request timed out".into(), NOW));
    }
    s.accounts.insert(Okx, stale);
    assert_eq!(reason(&s), "最新讀取失敗：request timed out");
}
