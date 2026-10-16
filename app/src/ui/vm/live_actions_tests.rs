use super::*;
use crate::engine::command::Command;
use crate::ui::bridge::EngineState;
use crate::ui::testkit::pair;
use std::cell::RefCell;
use tong_funding_core::risk::{ExecutionMode, TriggerMode};

#[derive(Default)]
struct Sink(RefCell<Vec<(String, Command)>>);
impl CommandSink for Sink {
    fn send(&self, label: String, command: Command) {
        self.0.borrow_mut().push((label, command));
    }
}

fn view(uuid: &str, id: &str, state: PairState, simulated: bool) -> PairView {
    PairView {
        internal_uuid: uuid.into(),
        pair_id: id.into(),
        symbol: "BTCUSDT".into(),
        long_exchange: Exchange::Binance,
        short_exchange: Exchange::Bybit,
        state,
        settlement_ms: 0,
        simulated,
        flat_confirmed: false,
    }
}

fn snap(identity: ProcessIdentity, state: PairState, simulated: bool) -> UiSnapshot {
    let mut s = UiSnapshot { identity, ..UiSnapshot::default() };
    s.engine = Some(EngineState { now_ms: 0, trigger_mode: TriggerMode::Manual, execution_mode: ExecutionMode::ExchangeDemo, pairs: vec![view("u-1", "p-1", state, simulated)], blockers: vec![], notices: vec![], alerts: vec![] });
    s.pairs = vec![pair("p-1", "BTCUSDT", Exchange::Binance, Exchange::Bybit, state)];
    s
}

#[test]
fn links_open_the_public_futures_pages_of_both_exchanges_in_live() {
    let l = exchange_links(ProcessIdentity::Live, "BTCUSDT", Exchange::Binance, Exchange::Bybit);
    assert_eq!(l.iter().map(|b| b.url.as_str()).collect::<Vec<_>>(), ["https://www.binance.com/en/futures/BTCUSDT", "https://www.bybit.com/trade/usdt/BTCUSDT"]);
    assert!(l[0].label.contains("Binance") && l[1].label.contains("Bybit"));
    // the same exchange twice gives one button; OKX has no live page
    assert_eq!(exchange_links(ProcessIdentity::Live, "BTCUSDT", Exchange::Bybit, Exchange::Bybit).len(), 1);
    assert_eq!(exchange_links(ProcessIdentity::Live, "BTCUSDT", Exchange::Okx, Exchange::Bybit).len(), 1);
    // a symbol that is not plain gets nothing
    assert!(exchange_links(ProcessIdentity::Live, "btc/usdt", Exchange::Binance, Exchange::Bybit).is_empty());
}

#[test]
fn demo_never_gets_link_or_flatten_buttons() {
    assert!(exchange_links(ProcessIdentity::Demo, "BTCUSDT", Exchange::Binance, Exchange::Bybit).is_empty());
    let s = snap(ProcessIdentity::Demo, PairState::PartialFailure, false);
    assert!(banner_links(&s, "p-1").is_empty());
    assert!(pair_links(&s, &s.pairs[0]).is_empty());
    assert!(flatten_buttons(&s, "p-1").is_empty());
}

#[test]
fn banner_links_follow_the_pair_of_the_alert() {
    let s = snap(ProcessIdentity::Live, PairState::PartialFailure, false);
    assert_eq!(banner_links(&s, "p-1").len(), 2);
    assert!(banner_links(&s, "store").is_empty(), "a halt alert has no pair, so no link");
}

#[test]
fn the_banner_items_of_a_locked_pair_carry_the_links_in_live_only() {
    use crate::ui::alerts::{Alert, Category};
    let alerts = vec![
        Alert { category: Category::ManualAttention, source: "p-1".into(), message: "m".into(), age_ms: None, link: None, fingerprint: String::new(), event_ms: None },
        Alert { category: Category::Halted, source: "store".into(), message: "h".into(), age_ms: None, link: None, fingerprint: String::new(), event_ms: None },
    ];
    let base = crate::ui::banner::banner(&alerts, &Default::default(), 0);
    assert!(base.items.iter().all(|i| i.links.is_empty()), "plain banner has no links");
    let live = crate::ui::banner::with_exchange_links(base.clone(), &snap(ProcessIdentity::Live, PairState::PartialFailure, false));
    let by = |cat: &str| live.items.iter().find(|i| i.category == cat).unwrap();
    assert_eq!(by(Category::ManualAttention.label()).links.len(), 2);
    assert!(by(Category::Halted.label()).links.is_empty());
    let demo = crate::ui::banner::with_exchange_links(base, &snap(ProcessIdentity::Demo, PairState::PartialFailure, false));
    assert!(demo.items.iter().all(|i| i.links.is_empty()));
}

#[test]
fn position_cards_of_locked_pairs_carry_links_and_flatten_in_live_only() {
    use crate::ui::positions::{build, Filter};
    for (identity, expect) in [(ProcessIdentity::Live, 2), (ProcessIdentity::Demo, 0)] {
        let s = snap(identity, PairState::Unresolved, false);
        let vm = build(&s, &Filter::default(), 0);
        let card = vm.pair_cards.iter().find(|c| c.pair_id == "p-1").expect("locked pair card");
        assert_eq!((card.links.len(), card.flatten.len()), (expect, expect), "{identity:?}");
    }
}

#[test]
fn flatten_buttons_exist_for_exactly_the_three_locked_states_in_live() {
    for st in [PairState::PartialFailure, PairState::Imbalanced, PairState::Unresolved] {
        let b = flatten_buttons(&snap(ProcessIdentity::Live, st, false), "p-1");
        assert_eq!(b.len(), 2, "{st:?}");
        assert_eq!((b[0].leg, b[0].exchange), (Leg::Long, Exchange::Binance));
        assert_eq!((b[1].leg, b[1].exchange), (Leg::Short, Exchange::Bybit));
        assert!(b[0].label.contains("Binance") && b[0].label.contains("多單") && b[1].label.contains("Bybit") && b[1].label.contains("空單"), "{b:?}");
        assert!(b.iter().all(|x| x.pair == "u-1"), "the engine's uuid, not the display id");
    }
    for st in [PairState::Reconciled, PairState::Closing, PairState::Prepared, PairState::OrderSubmit, PairState::Finalized, PairState::FillMonitor] {
        assert!(flatten_buttons(&snap(ProcessIdentity::Live, st, false), "p-1").is_empty(), "{st:?}");
    }
    assert!(flatten_buttons(&snap(ProcessIdentity::Live, PairState::PartialFailure, false), "other").is_empty());
}

#[test]
fn a_click_only_opens_a_confirmation_and_sending_needs_the_confirmation() {
    let s = snap(ProcessIdentity::Live, PairState::PartialFailure, false);
    let b = flatten_buttons(&s, "p-1").remove(1);
    let sink = Sink::default();
    let c = request_flatten(&s, &b).unwrap();
    assert!(sink.0.borrow().is_empty(), "requesting sends nothing");
    assert_eq!(c.button(), "以真錢平倉");
    assert!(c.env_line().contains("真錢") && c.question.contains("Bybit") && c.question.contains("reduce-only"), "{} / {}", c.env_line(), c.question);
    send_flatten(&s, &c, &sink).unwrap();
    assert_eq!(sink.0.borrow().len(), 1);
    assert_eq!(sink.0.borrow()[0].1, Command::FlattenLeg { pair: "u-1".into(), leg: Leg::Short });
}

#[test]
fn a_pair_that_left_the_locked_state_is_neither_confirmable_nor_sent() {
    let locked_snap = snap(ProcessIdentity::Live, PairState::Imbalanced, false);
    let b = flatten_buttons(&locked_snap, "p-1").remove(0);
    let c = request_flatten(&locked_snap, &b).unwrap();
    let moved = snap(ProcessIdentity::Live, PairState::Reconciled, false);
    assert!(request_flatten(&moved, &b).is_none());
    let sink = Sink::default();
    assert!(send_flatten(&moved, &c, &sink).is_err());
    assert!(sink.0.borrow().is_empty());
    let demo = snap(ProcessIdentity::Demo, PairState::Imbalanced, false);
    assert!(send_flatten(&demo, &c, &sink).is_err() && sink.0.borrow().is_empty());
}
