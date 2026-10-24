// Live scope: no OKX on any panel, and no manual opening (tasks 4 and 5 of the live UI work).
use crate::engine::command::PairView;
use crate::identity::ProcessIdentity;
use crate::ui::bridge::{EngineState, UiSnapshot};
use crate::ui::dashboard::ExchangeCard;
use crate::ui::manual_order::{build as manual_build, CancelForm, ManualForm, LIVE_OPEN_REFUSED};
use crate::ui::testkit::{asset, loaded, position};
use tong_funding_core::pair::PairState;
use tong_funding_core::risk::{ExecutionMode, TriggerMode};
use tong_funding_core::types::Exchange;

fn snap(identity: ProcessIdentity) -> UiSnapshot {
    let mut s = UiSnapshot { identity, ..UiSnapshot::default() };
    let pv = PairView { internal_uuid: "u".into(), pair_id: "p".into(), symbol: "BTCUSDT".into(), long_exchange: Exchange::Binance, short_exchange: Exchange::Bybit, state: PairState::Reconciled, settlement_ms: 0, simulated: false, flat_confirmed: false };
    s.engine = Some(EngineState { now_ms: 0, trigger_mode: TriggerMode::Manual, execution_mode: ExecutionMode::ExchangeDemo, pairs: vec![pv], blockers: vec![], notices: vec![], alerts: vec![] });
    for ex in Exchange::ALL {
        s.accounts.insert(ex, loaded(vec![asset("USDT", "100", Some("100"), None)], vec![position(ex, "BTCUSDT", "0.01", "60000", "60000", "5", "0", Some("120"))], 0));
    }
    s
}

#[test]
fn live_panels_and_account_cards_never_list_okx_and_demo_keeps_all_three() {
    for (identity, expect) in [(ProcessIdentity::Live, vec![Exchange::Binance, Exchange::Bybit]), (ProcessIdentity::Demo, Exchange::ALL.to_vec())] {
        let s = snap(identity);
        let m = manual_build(&ManualForm::default(), &CancelForm::default(), &s);
        assert_eq!(m.panels.iter().map(|p| p.0).collect::<Vec<_>>(), expect, "manual panels {identity:?}");
        let d = crate::ui::dashboard::build(&s, 0);
        let cards: Vec<Exchange> = d
            .cards
            .iter()
            .map(|c| match c {
                ExchangeCard::Connected(c) => c.exchange,
                ExchangeCard::NotConnected { exchange, .. } | ExchangeCard::Loading { exchange, .. } | ExchangeCard::Failed { exchange, .. } => *exchange,
                ExchangeCard::Disabled(n) => n.exchange,
            })
            .collect();
        assert_eq!(cards, expect, "dashboard cards {identity:?}");
        assert_eq!(d.exchanges_text, expect.iter().map(|e| e.name()).collect::<Vec<_>>().join(" + "));
        let p = crate::ui::positions::build(&s, &crate::ui::positions::Filter::default(), 0);
        assert_eq!(p.exchange_options, expect, "positions exchanges {identity:?}");
    }
}

#[test]
fn live_manual_order_keeps_only_reduce_only_and_cancel_and_says_why() {
    let live = snap(ProcessIdentity::Live);
    let open = ManualForm { quantity: "0.001".into(), ..ManualForm::default() };
    let vm = manual_build(&open, &CancelForm::default(), &live);
    assert!(!vm.open_allowed && vm.live_notice == Some(LIVE_OPEN_REFUSED));
    assert!(vm.submit_disabled.iter().any(|w| w == LIVE_OPEN_REFUSED), "{:?}", vm.submit_disabled);
    assert!(vm.live_notice.unwrap().contains("前往交易所"));
    assert_eq!(vm.links.len(), 2, "the typed symbol's pages on both exchanges");
    let ro = manual_build(&ManualForm { reduce_only: true, ..open.clone() }, &CancelForm::default(), &live);
    assert!(!ro.submit_disabled.iter().any(|w| w == LIVE_OPEN_REFUSED), "{:?}", ro.submit_disabled);
    let c = CancelForm { order_id: "x".into(), ..CancelForm::default() };
    assert!(manual_build(&open, &c, &live).cancel_disabled.is_empty());
    let dv = manual_build(&open, &CancelForm::default(), &snap(ProcessIdentity::Demo));
    assert!(dv.open_allowed && dv.live_notice.is_none() && dv.links.is_empty());
    assert!(!dv.submit_disabled.iter().any(|w| w == LIVE_OPEN_REFUSED));
}
