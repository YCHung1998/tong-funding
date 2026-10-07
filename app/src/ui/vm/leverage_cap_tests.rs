use super::*;
use crate::ui::bridge::CapReading;
use tong_funding_core::risk::ExecutionMode;

const NOW: i64 = 1_800_000_000_000;

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn snap(bin: Option<Result<&str, &str>>, byb: Option<Result<&str, &str>>, age_ms: i64, notional: &str) -> UiSnapshot {
    let mut s = UiSnapshot::default();
    for (ex, r) in [(Exchange::Binance, bin), (Exchange::Bybit, byb)] {
        if let Some(r) = r {
            s.leverage_caps.insert(
                (ex, "NMRUSDT".into()),
                CapReading { notional: d(notional), cap: r.map(d).map_err(String::from), fetched_at: NOW - age_ms },
            );
        }
    }
    s
}

fn chk(s: &UiSnapshot, lev: &str) -> CapCheck {
    check(s, Exchange::Binance, Exchange::Bybit, "NMRUSDT", d("1000"), d(lev), NOW)
}

#[test]
fn a_known_cap_below_the_leverage_exceeds_and_names_the_exchange() {
    let s = snap(Some(Ok("20")), Some(Ok("3")), 1_000, "1000");
    let c = chk(&s, "5");
    assert_eq!(c.verdict(), CapVerdict::Exceeds { exchange: Exchange::Bybit, cap: d("3") });
    for mode in [Some(ExecutionMode::ExchangeDemo), Some(ExecutionMode::Simulation), None] {
        assert_eq!(c.blocked_reason(mode).as_deref(), Some("槓桿 5× 超過 Bybit 上限 3×"), "{mode:?}");
    }
    assert!(c.text().contains("Binance 20×") && c.text().contains("Bybit 3×") && c.text().contains('✗'), "{}", c.text());
}

#[test]
fn caps_at_or_above_the_leverage_comply() {
    let s = snap(Some(Ok("5")), Some(Ok("50")), 1_000, "1000");
    let c = chk(&s, "5");
    assert_eq!(c.verdict(), CapVerdict::Complies);
    assert_eq!(c.blocked_reason(Some(ExecutionMode::ExchangeDemo)), None);
    assert!(c.text().ends_with('✓'));
}

#[test]
fn an_unknown_cap_blocks_in_exchange_demo_only() {
    let s = snap(Some(Ok("20")), Some(Err("timeout")), 1_000, "1000");
    let c = chk(&s, "5");
    assert!(matches!(c.verdict(), CapVerdict::Unknown { exchange: Exchange::Bybit, .. }));
    assert!(c.blocked_reason(Some(ExecutionMode::ExchangeDemo)).unwrap().contains("Bybit 槓桿上限未知"));
    assert_eq!(c.blocked_reason(Some(ExecutionMode::Simulation)), None, "informational in SIMULATION");
    assert!(c.text().contains("Bybit 未知"));
}

#[test]
fn a_known_exceeding_cap_wins_over_an_unknown_other_leg() {
    let s = snap(Some(Ok("2")), None, 1_000, "1000");
    assert_eq!(chk(&s, "5").verdict(), CapVerdict::Exceeds { exchange: Exchange::Binance, cap: d("2") });
}

#[test]
fn stale_never_read_and_other_notional_readings_are_unknown() {
    let stale = snap(Some(Ok("20")), Some(Ok("20")), CAP_FRESH_MS + 1, "1000");
    assert!(matches!(chk(&stale, "5").verdict(), CapVerdict::Unknown { .. }));
    let never = snap(None, None, 0, "1000");
    assert!(matches!(chk(&never, "5").verdict(), CapVerdict::Unknown { .. }));
    let smaller_notional = snap(Some(Ok("20")), Some(Ok("20")), 1_000, "500");
    let c = chk(&smaller_notional, "5");
    assert!(matches!(c.verdict(), CapVerdict::Unknown { .. }), "a reading for a smaller notional is no evidence: {}", c.text());
}

#[test]
fn a_reading_for_a_larger_notional_is_accepted_because_caps_only_shrink_with_size() {
    let larger = snap(Some(Ok("20")), Some(Ok("20")), 1_000, "5000");
    assert_eq!(chk(&larger, "5").verdict(), CapVerdict::Complies);
}

#[test]
fn single_leg_rules_match_the_pair_rules() {
    let known = LegCap::Known(d("3"));
    let demo = Some(ExecutionMode::ExchangeDemo);
    assert_eq!(leg_blocked_reason(Exchange::Bybit, &known, d("5"), Some(ExecutionMode::Simulation)).as_deref(), Some("槓桿 5× 超過 Bybit 上限 3×"));
    assert_eq!(leg_blocked_reason(Exchange::Bybit, &known, d("3"), demo), None);
    let unknown = LegCap::Unknown("尚未查詢".into());
    assert!(leg_blocked_reason(Exchange::Bybit, &unknown, d("5"), demo).unwrap().contains("Bybit 槓桿上限未知"));
    assert_eq!(leg_blocked_reason(Exchange::Bybit, &unknown, d("5"), Some(ExecutionMode::Simulation)), None);
    assert!(leg_text(Exchange::Bybit, &known, d("5")).contains('✗') && leg_text(Exchange::Bybit, &known, d("2")).ends_with('✓'));
}

#[test]
fn a_cap_exactly_fresh_enough_is_still_evidence() {
    let s = snap(Some(Ok("20")), Some(Ok("20")), CAP_FRESH_MS, "1000");
    assert_eq!(chk(&s, "5").verdict(), CapVerdict::Complies);
}
