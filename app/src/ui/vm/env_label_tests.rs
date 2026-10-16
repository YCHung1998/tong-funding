use super::*;

const IDENTITIES: [ProcessIdentity; 2] = [ProcessIdentity::Demo, ProcessIdentity::Live];
const MODES: [ExecutionMode; 2] = [ExecutionMode::Simulation, ExecutionMode::ExchangeDemo];

/// Adding an identity or a mode breaks THIS match first, so the arrays above are never stale.
#[allow(dead_code)]
fn exhaustive(i: ProcessIdentity, m: ExecutionMode) -> usize {
    (match i {
        ProcessIdentity::Demo => 0,
        ProcessIdentity::Live => 1,
    }) + (match m {
        ExecutionMode::Simulation => 0,
        ExecutionMode::ExchangeDemo => 2,
    })
}

fn all() -> Vec<EnvLabel> {
    IDENTITIES.into_iter().flat_map(|i| MODES.into_iter().map(move |m| EnvLabel::of(i, m))).collect()
}

fn live_words(s: &str) -> bool {
    s.contains("LIVE")
}

// ---- banner (spec: 每一頁固定顯示依身分的環境橫幅) ---------------------------------------------

#[test]
fn live_exchange_banner_says_live_real_money_and_production() {
    let l = EnvLabel::of(ProcessIdentity::Live, ExecutionMode::ExchangeDemo);
    let t = l.banner_text();
    assert!(t.contains("LIVE") && t.contains("真錢") && t.contains("Binance + Bybit 正式環境"), "{t}");
    assert_eq!(l.tone(), BannerTone::Live);
}

#[test]
fn live_simulation_banner_says_live_and_not_placing_orders() {
    let l = EnvLabel::of(ProcessIdentity::Live, ExecutionMode::Simulation);
    let t = l.banner_text();
    assert!(t.contains("LIVE") && t.contains("模擬（不下單）"), "{t}");
    assert!(!t.contains("真錢"), "a simulation must not claim real money: {t}");
    assert_eq!(l.tone(), BannerTone::Live);
}

#[test]
fn demo_banners_have_no_live_word_and_the_right_text() {
    let d = EnvLabel::of(ProcessIdentity::Demo, ExecutionMode::ExchangeDemo);
    assert!(d.banner_text().contains("DEMO") && d.banner_text().contains("不涉及真錢"));
    assert_eq!(d.tone(), BannerTone::Demo);
    let s = EnvLabel::of(ProcessIdentity::Demo, ExecutionMode::Simulation);
    assert!(s.banner_text().contains("模擬"));
    assert_eq!(s.tone(), BannerTone::Simulation);
}

#[test]
fn only_live_identity_ever_produces_live_wording() {
    for l in all() {
        let mut texts = vec![
            l.banner_text().to_string(),
            l.env_name().to_string(),
            l.mode_description().to_string(),
            l.staged_env_text().to_string(),
            l.manual_env_text().to_string(),
            l.ledger_source().to_string(),
            l.confirm_env_line(Some(Decimal::from(100))),
            l.confirm_env_line(None),
            l.mode_switch_text(),
            EnvLabel::title_prefix(l.identity).to_string(),
            EnvLabel::window_title(l.identity),
            EnvLabel::incomplete_settings_text(l.identity),
            EnvLabel::mode_unavailable_text(l.identity, "why"),
            EnvLabel::mode_option_name(l.identity, l.mode).to_string(),
            l.auto_add_consequence(TriggerMode::Auto),
            l.auto_add_consequence(TriggerMode::Manual),
        ];
        for k in [ConfirmKind::Enter, ConfirmKind::Close, ConfirmKind::ModeSwitch] {
            texts.push(l.confirm_button(k).to_string());
        }
        texts.extend(l.badge().map(String::from));
        for kill in [KillSwitchState::Off, KillSwitchState::On, KillSwitchState::Unknown] {
            let b = EnvBanner::build(l.identity, l.mode, kill);
            texts.push(b.text.to_string());
            texts.extend(b.kill_switch_text.map(String::from));
        }
        assert!(texts.iter().all(|t| !t.is_empty()), "empty text for {l:?}");
        if l.identity == ProcessIdentity::Demo {
            assert!(texts.iter().all(|t| !live_words(t)), "{l:?}");
        }
    }
}

#[test]
fn every_live_text_in_live_identity_names_live_for_the_real_mode() {
    let l = EnvLabel::of(ProcessIdentity::Live, ExecutionMode::ExchangeDemo);
    for t in [l.env_name(), l.staged_env_text(), l.manual_env_text(), l.ledger_source()] {
        assert!(live_words(t), "{t}");
    }
    assert!(l.confirm_env_line(Some(Decimal::from(1234))).contains("LIVE 真錢"));
}

// ---- kill switch -----------------------------------------------------------------------------

#[test]
fn live_banner_shows_the_kill_switch_state_in_words() {
    let armed = EnvBanner::build(ProcessIdentity::Live, ExecutionMode::ExchangeDemo, KillSwitchState::Off);
    assert_eq!(armed.kill_switch_text, Some("交易已武裝"));
    for locked in [KillSwitchState::On, KillSwitchState::Unknown] {
        let b = EnvBanner::build(ProcessIdentity::Live, ExecutionMode::ExchangeDemo, locked);
        assert_eq!(b.kill_switch_text, Some("交易已鎖定（只能平倉）"), "{locked:?} fails closed");
    }
    assert!(armed.text.contains("LIVE"));
}

#[test]
fn demo_banner_carries_no_kill_switch_wording() {
    for m in MODES {
        for k in [KillSwitchState::Off, KillSwitchState::On] {
            assert_eq!(EnvBanner::build(ProcessIdentity::Demo, m, k).kill_switch_text, None);
        }
    }
}

#[test]
fn banners_differ_between_live_and_demo_and_cannot_be_hidden() {
    let live = EnvBanner::build(ProcessIdentity::Live, ExecutionMode::ExchangeDemo, KillSwitchState::Off);
    let demo = EnvBanner::build(ProcessIdentity::Demo, ExecutionMode::ExchangeDemo, KillSwitchState::Off);
    assert_ne!(live, demo);
    // No visibility / dismiss state exists: destructuring every field proves that none was added.
    let EnvBanner { text, tone: _, kill_switch_text: _ } = live;
    assert!(!text.is_empty());
}

// ---- window title ----------------------------------------------------------------------------

#[test]
fn window_title_is_prefixed_by_identity() {
    assert!(EnvLabel::window_title(ProcessIdentity::Live).starts_with("[LIVE]"));
    assert!(EnvLabel::window_title(ProcessIdentity::Demo).starts_with("[DEMO]"));
    assert!(EnvLabel::window_title(ProcessIdentity::Live).contains("Funding Monitor"));
}

// ---- confirmations ---------------------------------------------------------------------------

#[test]
fn live_confirmation_buttons_say_real_money() {
    let l = EnvLabel::of(ProcessIdentity::Live, ExecutionMode::ExchangeDemo);
    assert_eq!(l.confirm_button(ConfirmKind::Enter), "以真錢下單");
    assert_eq!(l.confirm_button(ConfirmKind::Close), "以真錢平倉");
}

#[test]
fn demo_confirmation_keeps_the_existing_button_words() {
    for m in MODES {
        let l = EnvLabel::of(ProcessIdentity::Demo, m);
        assert_eq!(l.confirm_button(ConfirmKind::Enter), "確認送出");
        assert_eq!(l.confirm_button(ConfirmKind::Close), "確認平倉");
        assert_eq!(l.confirm_button(ConfirmKind::ModeSwitch), "確認切換");
    }
}

#[test]
fn confirmation_line_repeats_environment_and_notional() {
    let n = Some(Decimal::from(1234));
    let live = EnvLabel::of(ProcessIdentity::Live, ExecutionMode::ExchangeDemo).confirm_env_line(n);
    assert!(live.contains("LIVE 真錢") && live.contains("1,234.00"), "{live}");
    let demo = EnvLabel::of(ProcessIdentity::Demo, ExecutionMode::ExchangeDemo).confirm_env_line(n);
    assert!(demo.contains("DEMO") && demo.contains("1,234.00") && !demo.contains("真錢") && !demo.contains("LIVE"), "{demo}");
    let sim = EnvLabel::of(ProcessIdentity::Demo, ExecutionMode::Simulation).confirm_env_line(None);
    assert!(sim.contains("模擬") && sim.contains('—'), "{sim}");
}

// ---- demo / simulation wording unchanged -----------------------------------------------------

#[test]
fn demo_and_simulation_sentences_are_exactly_the_old_ones() {
    let sim = EnvLabel::of(ProcessIdentity::Demo, ExecutionMode::Simulation);
    let demo = EnvLabel::of(ProcessIdentity::Demo, ExecutionMode::ExchangeDemo);
    assert_eq!(sim.env_name(), "SIMULATION");
    assert_eq!(demo.env_name(), "EXCHANGE_DEMO");
    assert_eq!(sim.staged_env_text(), "SIMULATION：由模擬器成交，不會送出真實訂單");
    assert_eq!(demo.staged_env_text(), "EXCHANGE_DEMO：將對 demo / testnet 帳戶真實下單");
    assert_eq!(sim.manual_env_text(), "SIMULATION：由模擬器成交，不會送到交易所");
    assert_eq!(demo.manual_env_text(), "EXCHANGE_DEMO：將對 demo / testnet 帳戶真實下單");
    assert_eq!(sim.mode_description(), "跑完整流程與送單前檢查，訂單由模擬器成交，不送到任何交易所；不涉及真錢。");
    assert_eq!(demo.mode_description(), "對 demo / testnet 帳戶真實下單，會真的改變帳戶內的倉位；不涉及真錢。");
    assert_eq!(demo.mode_switch_text(), "確認切換到 EXCHANGE_DEMO？將對 demo / testnet 帳戶真實下單（不涉及真錢）。既有倉位與 trigger_mode 不變。");
    assert_eq!(EnvLabel::incomplete_settings_text(ProcessIdentity::Demo), "設定不完整時交易單與 EXCHANGE_DEMO 會被禁用");
    assert_eq!(EnvLabel::mode_unavailable_text(ProcessIdentity::Demo, "x"), "EXCHANGE_DEMO 不可選：x");
    assert_eq!(demo.auto_add_consequence(TriggerMode::Auto), "觸發模式 AUTO + EXCHANGE_DEMO：達標配對會自動加入，並依排程自動進場、真實下單到 demo / testnet 帳戶");
    assert_eq!(sim.auto_add_consequence(TriggerMode::Auto), "觸發模式 AUTO + SIMULATION：達標配對會自動加入並由模擬器自動進場");
    assert_eq!(demo.auto_add_consequence(TriggerMode::Manual), "觸發模式 MANUAL：只會自動加入交易單，進場仍需到交易單頁手動按下");
}

#[test]
fn live_never_describes_itself_as_demo() {
    for m in MODES {
        let l = EnvLabel::of(ProcessIdentity::Live, m);
        for t in [l.env_name(), l.staged_env_text(), l.manual_env_text(), l.ledger_source(), l.mode_description(), l.banner_text()] {
            assert!(!t.contains("EXCHANGE_DEMO") && !t.contains("demo") && !t.contains("DEMO"), "{l:?}: {t}");
        }
    }
    let live = EnvLabel::of(ProcessIdentity::Live, ExecutionMode::ExchangeDemo);
    assert!(live.ledger_source().contains("LIVE") && live.ledger_source().contains("交易所帳戶"));
    assert!(live.is_exchange());
    assert!(!EnvLabel::of(ProcessIdentity::Live, ExecutionMode::Simulation).is_exchange());
    assert!(live.badge() == Some("LIVE"));
    assert!(EnvLabel::of(ProcessIdentity::Demo, ExecutionMode::ExchangeDemo).badge().is_none());
}


// ---- from the snapshot -------------------------------------------------------------------------

fn snap_of(identity: ProcessIdentity, mode: Option<ExecutionMode>) -> crate::ui::bridge::UiSnapshot {
    use crate::ui::bridge::EngineState;
    let mut s = crate::ui::bridge::UiSnapshot { identity, ..Default::default() };
    s.engine = mode.map(|m| EngineState { now_ms: 0, trigger_mode: TriggerMode::Manual, execution_mode: m, pairs: vec![], blockers: vec![], notices: vec![], alerts: vec![] });
    s
}

#[test]
fn the_label_of_a_snapshot_uses_its_identity_and_the_engine_mode() {
    assert_eq!(EnvLabel::from_snapshot(&snap_of(ProcessIdentity::Live, Some(ExecutionMode::ExchangeDemo))), EnvLabel::of(ProcessIdentity::Live, ExecutionMode::ExchangeDemo));
    assert_eq!(EnvLabel::from_snapshot(&snap_of(ProcessIdentity::Demo, Some(ExecutionMode::Simulation))), EnvLabel::of(ProcessIdentity::Demo, ExecutionMode::Simulation));
    // A stopped engine reads as simulation (nothing can be sent), whatever the identity.
    assert_eq!(EnvLabel::from_snapshot(&snap_of(ProcessIdentity::Live, None)), EnvLabel::of(ProcessIdentity::Live, ExecutionMode::Simulation));
}

#[test]
fn the_banner_of_a_snapshot_follows_the_kill_switch() {
    let mut s = snap_of(ProcessIdentity::Live, Some(ExecutionMode::ExchangeDemo));
    s.system.kill_switch = KillSwitchState::On;
    assert_eq!(EnvBanner::of_snapshot(&s).kill_switch_text, Some("交易已鎖定（只能平倉）"));
    s.system.kill_switch = KillSwitchState::Off;
    let b = EnvBanner::of_snapshot(&s);
    assert!(b.text.contains("LIVE") && b.text.contains("真錢") && b.kill_switch_text == Some("交易已武裝"));
}

#[test]
fn the_header_chip_never_says_demo_in_a_live_process_nor_live_in_a_demo_one() {
    assert_eq!(EnvLabel::header_chip(ProcessIdentity::Demo), "Demo / Testnet");
    assert_eq!(EnvLabel::header_chip(ProcessIdentity::Live), "LIVE");
}
